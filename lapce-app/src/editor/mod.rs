use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    str::FromStr,
    sync::Arc,
    time::Duration
};

use anyhow::Result;
use doc::{
    EditorViewKind,
    lines::{
        ClickResult, RopeTextPosition,
        buffer::{
            InvalLines,
            diff::DiffLines,
            rope_text::{RopeText, RopeTextVal}
        },
        command::{
            EditCommand, FocusCommand, MotionModeCommand, MultiSelectionCommand,
            ScrollCommand
        },
        cursor::{Cursor, CursorMode},
        edit::EditType,
        editor_command::CommandExecuted,
        mode::{Mode, MotionMode},
        movement::Movement,
        selection::{InsertDrift, SelRegion, Selection}
    }
};
use floem::{
    ViewId,
    action::{TimerToken, exec_after, show_context_menu},
    ext_event::create_ext_action,
    keyboard::Modifiers,
    kurbo::{Point, Rect, Vec2},
    menu::{Menu, MenuItem},
    peniko::Color,
    pointer::{MouseButton, PointerButton, PointerInputEvent, PointerMoveEvent},
    reactive::{
        ReadSignal, RwSignal, Scope, SignalGet, SignalTrack, SignalUpdate,
        SignalWith, batch, use_context
    }
};
use lapce_core::{
    directory::Directory,
    doc::DocContent,
    editor_tab::EditorInfo,
    id::*,
    main_split::{SplitDirection, SplitMoveDirection},
    panel::PanelKind
};
use lapce_rpc::{plugin::PluginId, proxy::ProxyResponse};
use lapce_xi_rope::{Rope, RopeDelta, Transformer};
use log::error;
use lsp_types::{
    CodeActionResponse, CompletionItem, CompletionTextEdit, GotoDefinitionResponse,
    HoverContents, InlineCompletionTriggerKind, Location, MarkedString, MarkupKind,
    Range, TextEdit
};
use nucleo::Utf32Str;
use view::StickyHeaderInfo;

use self::location::{EditorLocation, EditorPosition};
use crate::{
    command::{CommandKind, InternalCommand, LapceCommand, LapceWorkbenchCommand},
    completion::CompletionStatus,
    config::color::LapceColor,
    db::LapceDb,
    doc::Doc,
    editor::{
        floem_editor::{Editor, do_motion_mode},
        movement::{do_multi_selection, move_cursor}
    },
    editor_tab::EditorTabChildId,
    inline_completion::{InlineCompletionItem, InlineCompletionStatus},
    keypress::{KeyPressFocus, condition::Condition},
    lsp::path_from_url,
    main_split::Editors,
    markdown::{
        MarkdownContent, from_marked_string, from_plaintext, parse_markdown
    },
    panel::{
        call_hierarchy_view::{CallHierarchyData, CallHierarchyItemData},
        implementation_view::{init_implementation_root, map_to_location}
    },
    snippet::Snippet,
    window_workspace::{CommonData, Focus, WindowWorkspaceData}
};

pub mod diff;
pub mod floem_editor;
pub mod gutter;
pub mod location;
pub mod view;

pub mod gutter_new;
pub mod movement;

#[derive(Clone, Debug)]
pub enum InlineFindDirection {
    Left,
    Right
}

// #[derive(Clone)]
// pub enum EditorViewKind {
//     Normal,
//     Diff(DiffInfo),
// }

// impl EditorViewKind {
//     pub fn is_normal(&self) -> bool {
//         matches!(self, EditorViewKind::Normal)
//     }
// }

#[derive(Clone)]
pub struct OnScreenFind {
    pub active:  bool,
    pub pattern: String,
    pub regions: Vec<SelRegion>
}

pub type SnippetIndex = Vec<(usize, (usize, usize))>;

/// Shares data between cloned instances as long as the signals aren't swapped
/// out.
#[derive(Clone, Debug)]
pub struct EditorData {
    pub scope:                Scope,
    pub editor_tab_id:        RwSignal<Option<EditorTabManageId>>,
    pub diff_editor_id:       RwSignal<Option<(EditorTabManageId, DiffEditorId)>>,
    pub confirmed:            RwSignal<bool>,
    pub snippet:              RwSignal<Option<SnippetIndex>>,
    pub inline_find:          RwSignal<Option<InlineFindDirection>>,
    pub on_screen_find:       RwSignal<OnScreenFind>,
    pub last_inline_find:     RwSignal<Option<(InlineFindDirection, String)>>,
    pub find_focus:           RwSignal<bool>,
    pub editor:               Rc<Editor>,
    // pub kind: RwSignal<EditorViewKind>,
    pub sticky_header_height: RwSignal<f64>,
    pub common:               Rc<CommonData>,
    pub sticky_header_info:   RwSignal<StickyHeaderInfo>,
    pub offset_line_from_top: RwSignal<Option<Option<usize>>>
}

impl PartialEq for EditorData {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl EditorData {
    fn new(
        cx: Scope,
        editor: Editor,
        editor_tab_id: Option<EditorTabManageId>,
        diff_editor_id: Option<(EditorTabManageId, DiffEditorId)>,
        confirmed: Option<RwSignal<bool>>,
        common: Rc<CommonData>
    ) -> Self {
        let cx = cx.create_child();

        let confirmed = confirmed.unwrap_or_else(|| cx.create_rw_signal(false));
        EditorData {
            scope: cx,
            editor_tab_id: cx.create_rw_signal(editor_tab_id),
            diff_editor_id: cx.create_rw_signal(diff_editor_id),
            confirmed,
            snippet: cx.create_rw_signal(None),
            inline_find: cx.create_rw_signal(None),
            on_screen_find: cx.create_rw_signal(OnScreenFind {
                active:  false,
                pattern: "".to_string(),
                regions: Vec::new()
            }),
            last_inline_find: cx.create_rw_signal(None),
            find_focus: cx.create_rw_signal(false),
            editor: Rc::new(editor),
            // kind: cx.create_rw_signal(EditorViewKind::Normal),
            sticky_header_height: cx.create_rw_signal(0.0),
            common,
            sticky_header_info: cx.create_rw_signal(StickyHeaderInfo::default()),
            offset_line_from_top: cx.create_rw_signal(None)
        }
    }

    pub fn kind(&self) -> ReadSignal<EditorViewKind> {
        self.doc().kind.read_only()
    }

    pub fn kind_rw(&self) -> RwSignal<EditorViewKind> {
        self.doc().kind
    }

    /// Create a new local editor.
    /// You should prefer calling [`Editors::make_local`] /
    /// [`Editors::new_local`] instead to register the editor.
    pub fn new_local(
        cx: Scope,
        editors: Editors,
        common: Rc<CommonData>,
        name: Option<String>
    ) -> Self {
        Self::new_local_id(cx, editors, common, name)
    }

    /// Create a new local editor with the given id.
    /// You should prefer calling [`Editors::make_local`] /
    /// [`Editors::new_local`] instead to register the editor.
    pub fn new_local_id(
        cx: Scope,
        editors: Editors,
        common: Rc<CommonData>,
        name: Option<String>
    ) -> Self {
        let cx = cx.create_child();
        let doc = Rc::new(Doc::new_local(cx, editors, common.clone(), name));
        let editor = doc.create_editor(cx, true);
        Self::new(cx, editor, None, None, None, common)
    }

    /// Create a new editor with a specific doc.
    /// You should prefer calling [`Editors::new_editor_doc`] /
    /// [`Editors::make_from_doc`] instead.
    pub fn new_doc(
        cx: Scope,
        doc: Rc<Doc>,
        editor_tab_id: Option<EditorTabManageId>,
        diff_editor_id: Option<(EditorTabManageId, DiffEditorId)>,
        confirmed: Option<RwSignal<bool>>,
        common: Rc<CommonData>
    ) -> Self {
        let editor = doc.create_editor(cx, false);
        Self::new(cx, editor, editor_tab_id, diff_editor_id, confirmed, common)
    }

    /// Swap out the document this editor is for
    pub fn update_doc(&self, doc: Rc<Doc>) {
        self.editor.update_doc(doc);
    }

    /// Create a new editor using the same underlying [`Doc`]
    pub fn copy(
        &self,
        cx: Scope,
        editor_tab_id: Option<EditorTabManageId>,
        diff_editor_id: Option<(EditorTabManageId, DiffEditorId)>,
        confirmed: Option<RwSignal<bool>>
    ) -> Self {
        let cx = cx.create_child();

        let confirmed = confirmed.unwrap_or_else(|| cx.create_rw_signal(true));

        let editor = Self::new_doc(
            cx,
            self.doc(),
            editor_tab_id,
            diff_editor_id,
            Some(confirmed),
            self.common.clone()
        );
        editor.editor.cursor.set(self.editor.cursor.get_untracked());
        // editor
        //     .editor
        //     .viewport
        //     .set(self.editor.viewport.get_untracked());
        let viewport = self.editor.doc().lines.with_untracked(|x| x.viewport());
        editor
            .editor
            .scroll_to
            .set(Some(viewport.origin().to_vec2()));
        editor
            .editor
            .last_movement
            .set(self.editor.last_movement.get_untracked());

        editor
    }

    pub fn id(&self) -> EditorId {
        self.editor.id()
    }

    pub fn editor_info(&self, _data: &WindowWorkspaceData) -> EditorInfo {
        let offset = self.cursor().get_untracked().offset();
        let scroll_offset = self.viewport().origin();
        let doc = self.doc();
        let is_pristine = doc.is_pristine();
        let unsaved = if is_pristine {
            None
        } else {
            Some(doc.lines.with_untracked(|b| b.buffer().to_string()))
        };
        EditorInfo {
            content: self.doc().content.get_untracked(),
            unsaved,
            offset,
            scroll_offset: (scroll_offset.x, scroll_offset.y)
        }
    }

    pub fn cursor(&self) -> RwSignal<Cursor> {
        self.editor.cursor
    }

    pub fn viewport(&self) -> Rect {
        self.editor.doc().lines.with_untracked(|x| x.viewport())
    }

    pub fn signal_viewport(&self) -> ReadSignal<Rect> {
        self.editor
            .doc()
            .lines
            .with_untracked(|x| x.signal_viewport())
    }

    pub fn window_origin(&self) -> RwSignal<Point> {
        self.editor.window_origin
    }

    pub fn scroll_delta(&self) -> RwSignal<Vec2> {
        self.editor.scroll_delta
    }

    pub fn scroll_to(&self) -> RwSignal<Option<Vec2>> {
        self.editor.scroll_to
    }

    pub fn active(&self) -> RwSignal<bool> {
        self.editor.active
    }

    // /// Get the line information for lines on the screen.
    // pub fn screen_lines(&self) -> RwSignal<ScreenLines> {
    //     self.editor.screen_lines
    // }

    pub fn doc(&self) -> Rc<Doc> {
        self.editor.doc()
    }

    /// The signal for the editor's document.
    pub fn doc_signal(&self) -> DocSignal {
        DocSignal {
            inner: self.editor.doc_signal()
        }
    }

    pub fn text(&self) -> Rope {
        self.editor.text()
    }

    pub fn rope_text(&self) -> RopeTextVal {
        self.editor.rope_text()
    }

    fn run_edit_command(&self, cmd: &EditCommand) -> Result<CommandExecuted> {
        log::debug!("{:?}", cmd);
        let doc = self.doc();
        let text = self.editor.rope_text();
        let is_local = doc.content.with_untracked(|content| content.is_local());
        let modal =
            self.editor.doc().lines.with_untracked(|x| x.modal()) && !is_local;
        let smart_tab = self
            .common
            .config
            .with_untracked(|config| config.editor.smart_tab);
        let doc_before_edit = text.text().clone();
        let mut cursor = self.editor.cursor.get_untracked();
        let mut register = self.common.register.get_untracked();

        let yank_data = if let CursorMode::Visual { .. } = &cursor.mode() {
            Some(cursor.yank(&text))
        } else {
            None
        };

        let deltas =
            batch(|| doc.do_edit(&mut cursor, cmd, modal, &mut register, smart_tab));

        if !deltas.is_empty() {
            if let Some(data) = yank_data {
                register.add_delete(data?);
            }
        }
        self.editor.cursor.set(cursor);
        self.editor.register.set(register);

        if show_completion(cmd, &doc_before_edit, &deltas) {
            self.update_completion(false);
        } else {
            self.cancel_completion();
        }

        if *cmd == EditCommand::InsertNewLine {
            // Cancel so that there's no flickering
            self.cancel_inline_completion();
            self.update_inline_completion(InlineCompletionTriggerKind::Automatic)?;
            self.quit_on_screen_find();
        } else if show_inline_completion(cmd) {
            self.update_inline_completion(InlineCompletionTriggerKind::Automatic)?;
        } else {
            self.cancel_inline_completion();
        }

        self.apply_deltas(&deltas);
        if let EditCommand::NormalMode = cmd {
            self.snippet.set(None);
            self.quit_on_screen_find();
        }
        self.check_auto_save();
        Ok(CommandExecuted::Yes)
    }

    fn run_motion_mode_command(
        &self,
        cmd: &MotionModeCommand,
        count: Option<usize>
    ) -> CommandExecuted {
        let count = count.unwrap_or(1);
        let motion_mode = match cmd {
            MotionModeCommand::MotionModeDelete => MotionMode::Delete { count },
            MotionModeCommand::MotionModeIndent => MotionMode::Indent,
            MotionModeCommand::MotionModeOutdent => MotionMode::Outdent,
            MotionModeCommand::MotionModeYank => MotionMode::Yank { count }
        };
        let mut cursor = self.editor.cursor.get_untracked();
        let mut register = self.common.register.get_untracked();

        do_motion_mode(
            &self.editor,
            &*self.doc(),
            &mut cursor,
            motion_mode,
            &mut register
        );

        self.editor.cursor.set(cursor);
        self.common.register.set(register);

        CommandExecuted::Yes
    }

    fn run_multi_selection_command(
        &self,
        cmd: &MultiSelectionCommand
    ) -> CommandExecuted {
        let mut cursor = self.editor.cursor.get_untracked();
        let rope_text = self.rope_text();
        let doc = self.doc();

        // This is currently special-cased in Lapce because floem editor does not
        // have 'find'
        match cmd {
            MultiSelectionCommand::SelectAllCurrent => {
                if let CursorMode::Insert(mut selection) = cursor.mode().clone() {
                    if !selection.is_empty() {
                        let find = doc.find();

                        let first = selection.first().unwrap();
                        let (start, end) = if first.is_caret() {
                            rope_text.select_word(first.start)
                        } else {
                            (first.min(), first.max())
                        };
                        let search_str = rope_text.slice_to_cow(start..end);
                        let case_sensitive = find.case_sensitive(false);
                        let multicursor_case_sensitive =
                            self.common.config.with_untracked(|config| {
                                config.editor.multicursor_case_sensitive
                            });
                        let case_sensitive =
                            multicursor_case_sensitive || case_sensitive;
                        // let search_whole_word =
                        // config.editor.multicursor_whole_words;
                        find.set_case_sensitive(case_sensitive);
                        find.set_find(&search_str);
                        let mut offset = 0;
                        while let Some((start, end)) =
                            find.next(rope_text.text(), offset, false, false)
                        {
                            offset = end;
                            selection.add_region(SelRegion::new(start, end, None));
                        }
                    }
                    cursor.set_insert(selection);
                }
            },
            MultiSelectionCommand::SelectNextCurrent => {
                if let CursorMode::Insert(mut selection) = cursor.mode().clone() {
                    if !selection.is_empty() {
                        let mut had_caret = false;
                        for region in selection.regions_mut() {
                            if region.is_caret() {
                                had_caret = true;
                                let (start, end) =
                                    rope_text.select_word(region.start);
                                region.start = start;
                                region.end = end;
                            }
                        }
                        if !had_caret {
                            let find = doc.find();

                            let r = selection.last_inserted().unwrap();
                            let search_str =
                                rope_text.slice_to_cow(r.min()..r.max());
                            let case_sensitive = find.case_sensitive(false);
                            let multicursor_case_sensitive =
                                self.common.config.with_untracked(|config| {
                                    config.editor.multicursor_case_sensitive
                                });
                            let case_sensitive =
                                multicursor_case_sensitive || case_sensitive;
                            // let search_whole_word =
                            // config.editor.multicursor_whole_words;
                            find.set_case_sensitive(case_sensitive);
                            find.set_find(&search_str);
                            let mut offset = r.max();
                            let mut seen = HashSet::new();
                            while let Some((start, end)) =
                                find.next(rope_text.text(), offset, false, true)
                            {
                                if !selection
                                    .regions()
                                    .iter()
                                    .any(|r| r.min() == start && r.max() == end)
                                {
                                    selection.add_region(SelRegion::new(
                                        start, end, None
                                    ));
                                    break;
                                }
                                if seen.contains(&end) {
                                    break;
                                }
                                offset = end;
                                seen.insert(offset);
                            }
                        }
                    }
                    cursor.set_insert(selection);
                }
            },
            MultiSelectionCommand::SelectSkipCurrent => {
                if let CursorMode::Insert(mut selection) = cursor.mode().clone() {
                    if !selection.is_empty() {
                        let r = selection.last_inserted().unwrap();
                        if r.is_caret() {
                            let (start, end) = rope_text.select_word(r.start);
                            selection.replace_last_inserted_region(SelRegion::new(
                                start, end, None
                            ));
                        } else {
                            let find = doc.find();

                            let search_str =
                                rope_text.slice_to_cow(r.min()..r.max());
                            find.set_find(&search_str);
                            let mut offset = r.max();
                            let mut seen = HashSet::new();
                            while let Some((start, end)) =
                                find.next(rope_text.text(), offset, false, true)
                            {
                                if !selection
                                    .regions()
                                    .iter()
                                    .any(|r| r.min() == start && r.max() == end)
                                {
                                    selection.replace_last_inserted_region(
                                        SelRegion::new(start, end, None)
                                    );
                                    break;
                                }
                                if seen.contains(&end) {
                                    break;
                                }
                                offset = end;
                                seen.insert(offset);
                            }
                        }
                    }
                    cursor.set_insert(selection);
                }
            },
            _ => {
                if let Err(err) = do_multi_selection(&self.editor, &mut cursor, cmd)
                {
                    error!("{err:?}");
                }
            }
        };

        self.editor.cursor.set(cursor);
        // self.cancel_signature();
        self.cancel_completion();
        self.cancel_inline_completion();
        CommandExecuted::Yes
    }

    fn run_move_command(
        &self,
        movement: &Movement,
        count: Option<usize>,
        mods: Modifiers
    ) -> CommandExecuted {
        self.common.hover.active.set(false);
        if movement.is_jump()
            && movement != &self.editor.last_movement.get_untracked()
        {
            let path = self
                .doc()
                .content
                .with_untracked(|content| content.path().cloned());
            if let Some(path) = path {
                let offset = self.cursor().with_untracked(|c| c.offset());
                let scroll_offset = self.viewport().origin().to_vec2();
                self.common.internal_command.send(
                    InternalCommand::SaveJumpLocation {
                        path,
                        offset,
                        scroll_offset
                    }
                );
            }
        }
        self.editor.last_movement.set(movement.clone());

        let mut cursor = self.cursor().get_untracked();
        self.common.register.update(|register| {
            if let Err(err) = move_cursor(
                &self.editor,
                &*self.doc(),
                &mut cursor,
                movement,
                count.unwrap_or(1),
                mods.shift(),
                register
            ) {
                error!("{:?}", err);
            }
        });

        self.editor.cursor.set(cursor);

        if self.snippet.with_untracked(|s| s.is_some()) {
            self.snippet.update(|snippet| {
                let offset = self.editor.cursor.get_untracked().offset();
                let mut within_region = false;
                for (_, (start, end)) in snippet.as_mut().unwrap() {
                    if offset >= *start && offset <= *end {
                        within_region = true;
                        break;
                    }
                }
                if !within_region {
                    *snippet = None;
                }
            })
        }
        self.cancel_completion();
        CommandExecuted::Yes
    }

    pub fn run_scroll_command(
        &self,
        cmd: &ScrollCommand,
        count: Option<usize>,
        mods: Modifiers
    ) -> CommandExecuted {
        let prev_completion_index = self
            .common
            .completion
            .with_untracked(|c| c.active.get_untracked());

        match cmd {
            ScrollCommand::PageUp => {
                self.editor.page_move(false, mods);
            },
            ScrollCommand::PageDown => {
                self.editor.page_move(true, mods);
            },
            ScrollCommand::ScrollUp => {
                self.scroll(false, count.unwrap_or(1), mods);
            },
            ScrollCommand::ScrollDown => {
                self.scroll(true, count.unwrap_or(1), mods);
            },
            // TODO:
            ScrollCommand::CenterOfWindow => {},
            ScrollCommand::TopOfWindow => {},
            ScrollCommand::BottomOfWindow => {}
        }

        let current_completion_index = self
            .common
            .completion
            .with_untracked(|c| c.active.get_untracked());

        if prev_completion_index != current_completion_index {
            self.common.completion.with_untracked(|c| {
                let cursor_offset = self.cursor().with_untracked(|c| c.offset());
                c.update_document_completion(self, cursor_offset);
            });
        }

        CommandExecuted::Yes
    }

    pub fn run_focus_command(
        &self,
        cmd: &FocusCommand,
        _count: Option<usize>,
        mods: Modifiers
    ) -> CommandExecuted {
        // TODO(minor): Evaluate whether we should split this into subenums,
        // such as actions specific to the actual editor pane, movement, and list
        // movement.
        let prev_completion_index = self
            .common
            .completion
            .with_untracked(|c| c.active.get_untracked());

        match cmd {
            FocusCommand::ModalClose => {
                self.cancel_completion();
            },
            FocusCommand::SplitVertical => {
                if let Some(editor_tab_id) =
                    self.editor_tab_id.read_only().get_untracked()
                {
                    self.common.internal_command.send(InternalCommand::Split {
                        direction: SplitDirection::Vertical,
                        editor_tab_id
                    });
                } else if let Some((editor_tab_id, _)) =
                    self.diff_editor_id.get_untracked()
                {
                    self.common.internal_command.send(InternalCommand::Split {
                        direction: SplitDirection::Vertical,
                        editor_tab_id
                    });
                } else {
                    return CommandExecuted::No;
                }
            },
            FocusCommand::SplitHorizontal => {
                if let Some(editor_tab_id) = self.editor_tab_id.get_untracked() {
                    self.common.internal_command.send(InternalCommand::Split {
                        direction: SplitDirection::Horizontal,
                        editor_tab_id
                    });
                } else if let Some((editor_tab_id, _)) =
                    self.diff_editor_id.get_untracked()
                {
                    self.common.internal_command.send(InternalCommand::Split {
                        direction: SplitDirection::Horizontal,
                        editor_tab_id
                    });
                } else {
                    return CommandExecuted::No;
                }
            },
            FocusCommand::SplitRight => {
                if let Some(editor_tab_id) = self.editor_tab_id.get_untracked() {
                    self.common
                        .internal_command
                        .send(InternalCommand::SplitMove {
                            direction: SplitMoveDirection::Right,
                            editor_tab_id
                        });
                } else if let Some((editor_tab_id, _)) =
                    self.diff_editor_id.get_untracked()
                {
                    self.common
                        .internal_command
                        .send(InternalCommand::SplitMove {
                            direction: SplitMoveDirection::Right,
                            editor_tab_id
                        });
                } else {
                    return CommandExecuted::No;
                }
            },
            FocusCommand::SplitLeft => {
                if let Some(editor_tab_id) = self.editor_tab_id.get_untracked() {
                    self.common
                        .internal_command
                        .send(InternalCommand::SplitMove {
                            direction: SplitMoveDirection::Left,
                            editor_tab_id
                        });
                } else if let Some((editor_tab_id, _)) =
                    self.diff_editor_id.get_untracked()
                {
                    self.common
                        .internal_command
                        .send(InternalCommand::SplitMove {
                            direction: SplitMoveDirection::Left,
                            editor_tab_id
                        });
                } else {
                    return CommandExecuted::No;
                }
            },
            FocusCommand::SplitUp => {
                if let Some(editor_tab_id) = self.editor_tab_id.get_untracked() {
                    self.common
                        .internal_command
                        .send(InternalCommand::SplitMove {
                            direction: SplitMoveDirection::Up,
                            editor_tab_id
                        });
                } else if let Some((editor_tab_id, _)) =
                    self.diff_editor_id.get_untracked()
                {
                    self.common
                        .internal_command
                        .send(InternalCommand::SplitMove {
                            direction: SplitMoveDirection::Up,
                            editor_tab_id
                        });
                } else {
                    return CommandExecuted::No;
                }
            },
            FocusCommand::SplitDown => {
                if let Some(editor_tab_id) = self.editor_tab_id.get_untracked() {
                    self.common
                        .internal_command
                        .send(InternalCommand::SplitMove {
                            direction: SplitMoveDirection::Down,
                            editor_tab_id
                        });
                } else if let Some((editor_tab_id, _)) =
                    self.diff_editor_id.get_untracked()
                {
                    self.common
                        .internal_command
                        .send(InternalCommand::SplitMove {
                            direction: SplitMoveDirection::Down,
                            editor_tab_id
                        });
                } else {
                    return CommandExecuted::No;
                }
            },
            FocusCommand::SplitExchange => {
                if let Some(editor_tab_id) = self.editor_tab_id.get_untracked() {
                    self.common
                        .internal_command
                        .send(InternalCommand::SplitExchange { editor_tab_id });
                } else if let Some((editor_tab_id, _)) =
                    self.diff_editor_id.get_untracked()
                {
                    self.common
                        .internal_command
                        .send(InternalCommand::SplitExchange { editor_tab_id });
                } else {
                    return CommandExecuted::No;
                }
            },
            FocusCommand::SplitClose => {
                if let Some(editor_tab_id) = self.editor_tab_id.get_untracked() {
                    self.common.internal_command.send(
                        InternalCommand::EditorTabChildClose {
                            editor_tab_id,
                            child: EditorTabChildId::Editor(self.id())
                        }
                    );
                } else if let Some((editor_tab_id, diff_editor_id)) =
                    self.diff_editor_id.get_untracked()
                {
                    self.common.internal_command.send(
                        InternalCommand::EditorTabChildClose {
                            editor_tab_id,
                            child: EditorTabChildId::DiffEditor(diff_editor_id)
                        }
                    );
                } else {
                    return CommandExecuted::No;
                }
            },
            FocusCommand::ListNext => {
                self.common.completion.update(|c| {
                    c.next();
                });
            },
            FocusCommand::ListPrevious => {
                self.common.completion.update(|c| {
                    c.previous();
                });
            },
            FocusCommand::ListNextPage => {
                self.common.completion.update(|c| {
                    c.next_page();
                });
            },
            FocusCommand::ListPreviousPage => {
                self.common.completion.update(|c| {
                    c.previous_page();
                });
            },
            FocusCommand::ListSelect => {
                self.select_completion();
                self.cancel_inline_completion();
            },
            FocusCommand::JumpToNextSnippetPlaceholder => {
                self.snippet.update(|snippet| {
                    if let Some(snippet_mut) = snippet.as_mut() {
                        let mut current = 0;
                        let offset = self.cursor().get_untracked().offset();
                        for (i, (_, (start, end))) in snippet_mut.iter().enumerate()
                        {
                            if *start <= offset && offset <= *end {
                                current = i;
                                break;
                            }
                        }

                        let last_placeholder = current + 1 >= snippet_mut.len() - 1;

                        if let Some((_, (start, end))) = snippet_mut.get(current + 1)
                        {
                            let mut selection = Selection::new();
                            let region = SelRegion::new(*start, *end, None);
                            selection.add_region(region);
                            self.cursor().update(|cursor| {
                                cursor.set_insert(selection);
                            });
                        }

                        if last_placeholder {
                            *snippet = None;
                        }
                        // self.update_signature();
                        self.cancel_completion();
                        self.cancel_inline_completion();
                    }
                });
            },
            FocusCommand::JumpToPrevSnippetPlaceholder => {
                self.snippet.update(|snippet| {
                    if let Some(snippet_mut) = snippet.as_mut() {
                        let mut current = 0;
                        let offset = self.cursor().get_untracked().offset();
                        for (i, (_, (start, end))) in snippet_mut.iter().enumerate()
                        {
                            if *start <= offset && offset <= *end {
                                current = i;
                                break;
                            }
                        }

                        if current > 0 {
                            if let Some((_, (start, end))) =
                                snippet_mut.get(current - 1)
                            {
                                let mut selection = Selection::new();
                                let region = SelRegion::new(*start, *end, None);
                                selection.add_region(region);
                                self.cursor().update(|cursor| {
                                    cursor.set_insert(selection);
                                });
                            }
                            // self.update_signature();
                            self.cancel_completion();
                            self.cancel_inline_completion();
                        }
                    }
                });
            },
            FocusCommand::GotoDefinition => {
                if let Err(err) = self.go_to_definition() {
                    error!("{:?}", err);
                }
            },
            FocusCommand::ShowCodeActions => {
                self.show_code_actions(false);
            },
            FocusCommand::SearchWholeWordForward => {
                self.search_whole_word_forward(mods);
            },
            FocusCommand::SearchForward => {
                self.search_forward(mods);
            },
            FocusCommand::SearchBackward => {
                self.search_backward(mods);
            },
            FocusCommand::Save => {
                self.save(true, || {});
            },
            FocusCommand::SaveWithoutFormatting => {
                self.save(false, || {});
            },
            FocusCommand::FormatDocument => {
                self.format();
            },
            FocusCommand::InlineFindLeft => {
                self.inline_find.set(Some(InlineFindDirection::Left));
            },
            FocusCommand::InlineFindRight => {
                self.inline_find.set(Some(InlineFindDirection::Right));
            },
            FocusCommand::OnScreenFind => {
                self.on_screen_find.update(|find| {
                    find.active = true;
                    find.pattern.clear();
                    find.regions.clear();
                });
            },
            FocusCommand::RepeatLastInlineFind => {
                if let Some((direction, c)) = self.last_inline_find.get_untracked() {
                    if let Err(err) = self.inline_find(direction, &c) {
                        error!("{:?}", err);
                    }
                }
            },
            FocusCommand::Rename => {
                if let Err(err) = self.rename() {
                    error!("{err:?}");
                }
            },
            FocusCommand::ClearSearch => {
                self.clear_search();
            },
            FocusCommand::Search => {
                self.search();
            },
            FocusCommand::FocusFindEditor => {
                self.common.find.replace_focus.set(false);
            },
            FocusCommand::FocusReplaceEditor => {
                if self.common.find.replace_active.get_untracked() {
                    self.common.find.replace_focus.set(true);
                }
            },
            FocusCommand::InlineCompletionSelect => {
                // todo!("check to save");
                self.select_inline_completion();
                self.check_auto_save();
            },
            FocusCommand::InlineCompletionNext => {
                self.next_inline_completion();
            },
            FocusCommand::InlineCompletionPrevious => {
                self.previous_inline_completion();
            },
            FocusCommand::InlineCompletionCancel => {
                self.cancel_inline_completion();
            },
            FocusCommand::InlineCompletionInvoke => {
                if let Err(err) = self
                    .update_inline_completion(InlineCompletionTriggerKind::Invoked)
                {
                    error!("{err:?}");
                }
            },
            FocusCommand::ShowHover => {
                let start_offset = self.doc().lines.with_untracked(|b| {
                    b.buffer()
                        .prev_code_boundary(self.cursor().get_untracked().offset())
                });
                self.update_hover(start_offset);
            },
            _ => {}
        }

        let current_completion_index = self
            .common
            .completion
            .with_untracked(|c| c.active.get_untracked());

        if prev_completion_index != current_completion_index {
            self.common.completion.with_untracked(|c| {
                let cursor_offset = self.cursor().with_untracked(|c| c.offset());
                c.update_document_completion(self, cursor_offset);
            });
        }

        CommandExecuted::Yes
    }

    /// Jump to the next/previous column on the line which matches the given
    /// text
    fn inline_find(&self, direction: InlineFindDirection, c: &str) -> Result<()> {
        let offset = self.cursor().with_untracked(|c| c.offset());
        let doc = self.doc();
        let (line_content, line_start_offset) = doc.lines.with_untracked(|b| {
            let line = b.buffer().line_of_offset(offset);
            let line_content = b.buffer().line_content(line)?;
            let line_start_offset = b.buffer().offset_of_line(line)?;
            Ok::<(String, usize), anyhow::Error>((
                line_content.to_string(),
                line_start_offset
            ))
        })?;
        let index = offset - line_start_offset;
        if let Some(new_index) = match direction {
            InlineFindDirection::Left => line_content[..index].rfind(c),
            InlineFindDirection::Right => {
                if index + 1 >= line_content.len() {
                    None
                } else {
                    let index = index
                        + doc.lines.with_untracked(|b| {
                            Ok::<usize, anyhow::Error>(
                                b.buffer().next_grapheme_offset(
                                    offset,
                                    1,
                                    b.buffer().offset_line_end(offset, false)?
                                )
                            )
                        })?
                        - offset;
                    line_content[index..].find(c).map(|i| i + index)
                }
            },
        } {
            self.run_move_command(
                &Movement::Offset(new_index + line_start_offset),
                None,
                Modifiers::empty()
            );
        }
        Ok(())
    }

    fn quit_on_screen_find(&self) {
        if self.on_screen_find.with_untracked(|s| s.active) {
            self.on_screen_find.update(|f| {
                f.active = false;
                f.pattern.clear();
                f.regions.clear();
            })
        }
    }

    fn on_screen_find(&self, pattern: &str) -> Vec<SelRegion> {
        let screen_lines = self
            .editor
            .doc()
            .lines
            .with_untracked(|x| x.screen_lines().clone());
        let lines: HashSet<usize> = screen_lines
            .visual_lines
            .iter()
            .map(|l| l.visual_line.origin_line)
            .collect();

        let mut matcher = nucleo::Matcher::new(nucleo::Config::DEFAULT);
        let pattern = nucleo::pattern::Pattern::parse(
            pattern,
            nucleo::pattern::CaseMatching::Ignore,
            nucleo::pattern::Normalization::Smart
        );
        let mut indices = Vec::new();
        let mut filter_text_buf = Vec::new();
        let mut items = Vec::new();

        let buffer = self.doc().lines.with_untracked(|b| b.signal_buffer());

        for line in lines {
            filter_text_buf.clear();
            indices.clear();

            if let Err(err) = buffer.with_untracked(|buffer| {
                let start = buffer.offset_of_line(line)?;
                let end = buffer.offset_of_line(line + 1)?;
                let text = buffer.text().slice_to_cow(start..end);
                let filter_text = Utf32Str::new(&text, &mut filter_text_buf);

                if let Some(score) =
                    pattern.indices(filter_text, &mut matcher, &mut indices)
                {
                    indices.sort();
                    let left =
                        start + indices.first().copied().unwrap_or(0) as usize;
                    let right =
                        start + indices.last().copied().unwrap_or(0) as usize + 1;
                    let right = if right == left { left + 1 } else { right };
                    items.push((score, left, right));
                }
                Ok::<(), anyhow::Error>(())
            }) {
                error!("{err:?}");
            }
        }

        items.sort_by_key(|(score, _, _)| -(*score as i64));
        if let Some((_, offset, _)) = items.first().copied() {
            self.run_move_command(
                &Movement::Offset(offset),
                None,
                Modifiers::empty()
            );
        }

        items
            .into_iter()
            .map(|(_, start, end)| SelRegion::new(start, end, None))
            .collect()
    }

    fn go_to_definition(&self) -> Result<()> {
        let doc = self.doc();
        let path = match if doc.loaded() {
            doc.content.with_untracked(|c| c.path().cloned())
        } else {
            None
        } {
            Some(path) => path,
            None => return Ok(())
        };

        let offset = self.cursor().with_untracked(|c| c.offset());
        let (start_position, position) = doc.lines.with_untracked(|b| {
            let start_offset = b.buffer().prev_code_boundary(offset);
            let start_position = b.buffer().offset_to_position(start_offset);
            let position = b.buffer().offset_to_position(offset);
            (start_position, position)
        });
        let (start_position, position) = (start_position?, position?);

        enum DefinitionOrReferece {
            Location(EditorLocation),
            References(Vec<Location>)
        }

        let internal_command = self.common.internal_command;
        let cursor = self.cursor().read_only();
        let send = create_ext_action(self.scope, move |d| {
            let current_offset = cursor.with_untracked(|c| c.offset());
            if current_offset != offset {
                return;
            }

            match d {
                DefinitionOrReferece::Location(location) => {
                    internal_command
                        .send(InternalCommand::JumpToLocation { location });
                },
                DefinitionOrReferece::References(locations) => {
                    internal_command.send(InternalCommand::PaletteReferences {
                        references: locations
                            .into_iter()
                            .map(|l| EditorLocation {
                                path:               path_from_url(&l.uri),
                                position:           Some(EditorPosition::Position(
                                    l.range.start
                                )),
                                scroll_offset:      None,
                                ignore_unconfirmed: false,
                                same_editor_tab:    false
                            })
                            .collect()
                    });
                }
            }
        });
        let proxy = self.common.proxy.clone();
        self.common.proxy.get_definition(
            offset,
            path.clone(),
            position,
            move |(_, result)| {
                if let Ok(ProxyResponse::GetDefinitionResponse {
                    definition, ..
                }) = result
                {
                    if let Some(location) = match definition {
                        GotoDefinitionResponse::Scalar(location) => Some(location),
                        GotoDefinitionResponse::Array(locations) => {
                            if !locations.is_empty() {
                                Some(locations[0].clone())
                            } else {
                                None
                            }
                        },
                        GotoDefinitionResponse::Link(location_links) => {
                            let location_link = location_links[0].clone();
                            Some(Location {
                                uri:   location_link.target_uri,
                                range: location_link.target_selection_range
                            })
                        }
                    } {
                        if location.range.start == start_position {
                            proxy.get_references(
                                path.clone(),
                                position,
                                move |(_, result)| {
                                    if let Ok(
                                        ProxyResponse::GetReferencesResponse {
                                            references
                                        }
                                    ) = result
                                    {
                                        if references.is_empty() {
                                            return;
                                        }
                                        if references.len() == 1 {
                                            let location = &references[0];
                                            send(DefinitionOrReferece::Location(
                                                EditorLocation {
                                                    path:
                                                        path_from_url(&location.uri),
                                                    position:           Some(
                                                        EditorPosition::Position(
                                                            location.range.start
                                                        )
                                                    ),
                                                    scroll_offset:      None,
                                                    ignore_unconfirmed: false,
                                                    same_editor_tab:    false
                                                }
                                            ));
                                        } else {
                                            send(DefinitionOrReferece::References(
                                                references
                                            ));
                                        }
                                    }
                                }
                            );
                        } else {
                            let path = path_from_url(&location.uri);
                            send(DefinitionOrReferece::Location(EditorLocation {
                                path,
                                position: Some(EditorPosition::Position(
                                    location.range.start
                                )),
                                scroll_offset: None,
                                ignore_unconfirmed: false,
                                same_editor_tab: false
                            }));
                        }
                    }
                }
            }
        );
        Ok(())
    }

    pub fn call_hierarchy(
        &self,
        window_tab_data: WindowWorkspaceData
    ) -> Result<()> {
        let doc = self.doc();
        let path = match if doc.loaded() {
            doc.content.with_untracked(|c| c.path().cloned())
        } else {
            None
        } {
            Some(path) => path,
            None => return Ok(())
        };

        let offset = self.cursor().with_untracked(|c| c.offset());
        let (_start_position, position) = doc.lines.with_untracked(|b| {
            let start_offset = b.buffer().prev_code_boundary(offset);
            let start_position = b.buffer().offset_to_position(start_offset);
            let position = b.buffer().offset_to_position(offset);
            (start_position, position)
        });
        let (_start_position, position) = (_start_position?, position?);

        let scope = window_tab_data.scope;
        let range = Range {
            start: _start_position,
            end:   position
        };
        self.common.proxy.show_call_hierarchy(
            path,
            position,
            create_ext_action(self.scope, move |(_, result)| {
                if let Ok(ProxyResponse::ShowCallHierarchyResponse {
                    items, ..
                }) = result
                {
                    if let Some(item) = items.and_then(|x| x.into_iter().next()) {
                        let root_id = ViewId::new();
                        let name = item.name.clone();
                        let root = CallHierarchyItemData {
                            root_id,
                            view_id: root_id,
                            item: Rc::new(item),
                            from_range: range,
                            init: false,
                            open: scope.create_rw_signal(true),
                            children: scope.create_rw_signal(Vec::with_capacity(0))
                        };
                        let root = window_tab_data
                            .main_split
                            .hierarchy
                            .cx
                            .create_rw_signal(root);
                        window_tab_data.main_split.hierarchy.push_tab(
                            name,
                            CallHierarchyData {
                                root,
                                root_id,
                                scroll_to_line: None
                            }
                        );
                        // call_hierarchy_data.root.update(|x| {
                        //     *x = Some(root);
                        // });
                        window_tab_data.show_panel(PanelKind::CallHierarchy);
                        window_tab_data.common.internal_command.send(
                            InternalCommand::CallHierarchyIncoming {
                                item_id: root_id,
                                root_id
                            }
                        );
                    }
                }
            })
        );
        Ok(())
    }

    pub fn fold_code(&self) -> Result<()> {
        let doc = self.doc();
        if !doc.content.get_untracked().is_file() {
            return Ok(());
        }

        let offset = self.cursor().with_untracked(|c| c.offset());
        doc.lines.update(|x| {
            if let Err(e) = x.update_folding_ranges(offset.into()) {
                error!("{:?}", e);
            }
        });
        Ok(())
    }

    pub fn find_refenrence(
        &self,
        window_tab_data: WindowWorkspaceData
    ) -> Result<()> {
        let doc = self.doc();
        let path = match if doc.loaded() {
            doc.content.with_untracked(|c| c.path().cloned())
        } else {
            None
        } {
            Some(path) => path,
            None => return Ok(())
        };

        let offset = self.cursor().with_untracked(|c| c.offset());
        let (_start_position, position, symbol) = doc.lines.with_untracked(|b| {
            let start_offset = b.buffer().prev_code_boundary(offset);
            let end_offset = b.buffer().next_code_boundary(offset);
            let start_position = b.buffer().offset_to_position(start_offset);
            let position = b.buffer().offset_to_position(offset);
            let symbol = b
                .buffer()
                .slice_to_cow(start_offset..end_offset)
                .to_string();
            (start_position, position, symbol)
        });
        let position = position?;
        let scope = window_tab_data.scope;
        let update_implementation = create_ext_action(self.scope, {
            let window_tab_data = window_tab_data.clone();
            move |(_, result)| {
                if let Ok(ProxyResponse::ReferencesResolveResponse { items }) =
                    result
                {
                    window_tab_data
                        .main_split
                        .references
                        .push_tab(symbol, init_implementation_root(items, scope));
                    window_tab_data.show_panel(PanelKind::References);
                }
            }
        });
        let proxy = self.common.proxy.clone();
        self.common.proxy.get_references(
            path,
            position,
            create_ext_action(self.scope, move |(_, result)| {
                if let Ok(ProxyResponse::GetReferencesResponse { references }) =
                    result
                {
                    {
                        if !references.is_empty() {
                            proxy.references_resolve(
                                references,
                                update_implementation
                            );
                        } else {
                            window_tab_data.show_panel(PanelKind::References);
                        }
                    }
                }
            })
        );
        Ok(())
    }

    pub fn go_to_implementation(
        &self,
        window_tab_data: WindowWorkspaceData
    ) -> Result<()> {
        let doc = self.doc();
        let path = match if doc.loaded() {
            doc.content.with_untracked(|c| c.path().cloned())
        } else {
            None
        } {
            Some(path) => path,
            None => return Ok(())
        };

        let offset = self.cursor().with_untracked(|c| c.offset());
        let (_start_position, position, symbol) = doc.lines.with_untracked(|b| {
            let start_offset = b.buffer().prev_code_boundary(offset);
            let end_offset = b.buffer().next_code_boundary(offset);
            let start_position = b.buffer().offset_to_position(start_offset);
            let position = b.buffer().offset_to_position(offset);
            let symbol = b
                .buffer()
                .slice_to_cow(start_offset..end_offset)
                .to_string();
            (start_position, position, symbol)
        });
        let position = position?;
        let scope = window_tab_data.scope;
        let update_implementation = create_ext_action(self.scope, {
            let window_tab_data = window_tab_data.clone();
            move |(_, result)| {
                if let Ok(ProxyResponse::ReferencesResolveResponse { items }) =
                    result
                {
                    window_tab_data
                        .main_split
                        .implementations
                        .push_tab(symbol, init_implementation_root(items, scope));
                    window_tab_data.show_panel(PanelKind::Implementation);
                }
            }
        });
        let proxy = self.common.proxy.clone();
        self.common.proxy.go_to_implementation(
            path,
            position,
            create_ext_action(self.scope, {
                move |(_, result)| {
                    if let Ok(ProxyResponse::GotoImplementationResponse {
                        resp,
                        ..
                    }) = result
                    {
                        let locations = map_to_location(resp);
                        if !locations.is_empty() {
                            proxy.references_resolve(
                                locations,
                                update_implementation
                            );
                        } else {
                            window_tab_data.show_panel(PanelKind::Implementation);
                        }
                    }
                }
            })
        );
        Ok(())
    }

    fn scroll(&self, down: bool, count: usize, mods: Modifiers) {
        self.editor.scroll(
            self.sticky_header_height.get_untracked(),
            down,
            count,
            mods
        )
    }

    fn select_inline_completion(&self) {
        if self
            .common
            .inline_completion
            .with_untracked(|c| c.status == InlineCompletionStatus::Inactive)
        {
            return;
        }

        let data = self
            .common
            .inline_completion
            .with_untracked(|c| (c.current_item().cloned(), c.start_offset));
        self.cancel_inline_completion();

        let (Some(item), start_offset) = data else {
            return;
        };

        if let Err(err) = item.apply(self, start_offset) {
            log::error!("{:?}", err);
        }
        self.check_auto_save();
    }

    fn next_inline_completion(&self) {
        if self
            .common
            .inline_completion
            .with_untracked(|c| c.status == InlineCompletionStatus::Inactive)
        {
            return;
        }

        self.common.inline_completion.update(|c| {
            c.next();
        });
    }

    fn previous_inline_completion(&self) {
        if self
            .common
            .inline_completion
            .with_untracked(|c| c.status == InlineCompletionStatus::Inactive)
        {
            return;
        }

        self.common.inline_completion.update(|c| {
            c.previous();
        });
    }

    pub fn cancel_inline_completion(&self) {
        if self
            .common
            .inline_completion
            .with_untracked(|c| c.status == InlineCompletionStatus::Inactive)
        {
            return;
        }

        self.common.inline_completion.update(|c| {
            c.cancel();
        });

        self.doc().clear_inline_completion();
    }

    /// Update the current inline completion
    fn update_inline_completion(
        &self,
        trigger_kind: InlineCompletionTriggerKind
    ) -> Result<()> {
        if self.get_mode() != Mode::Insert {
            self.cancel_inline_completion();
            return Ok(());
        }

        let doc = self.doc();
        let path = match if doc.loaded() {
            doc.content.with_untracked(|c| c.path().cloned())
        } else {
            None
        } {
            Some(path) => path,
            None => return Ok(())
        };

        let offset = self.cursor().with_untracked(|c| c.offset());
        let (line, position) = doc.lines.with_untracked(|b| {
            (
                b.buffer().line_of_offset(offset),
                b.buffer().offset_to_position(offset)
            )
        });
        let position = position?;
        // let position = doc
        //     .buffer
        //     .with_untracked(|buffer| buffer.offset_to_position(offset));

        let inline_completion = self.common.inline_completion;
        let doc = self.doc();

        // Update the inline completion's text if it's already active to avoid
        // flickering
        let has_relevant = inline_completion.with_untracked(|completion| {
            let c_line = doc.lines.with_untracked(|b| {
                b.buffer().line_of_offset(completion.start_offset)
            });
            completion.status != InlineCompletionStatus::Inactive
                && line == c_line
                && completion.path == path
        });
        if has_relevant {
            let enable_inline_completion = self
                .common
                .config
                .with_untracked(|config| config.editor.enable_inline_completion);
            inline_completion.update(|completion| {
                completion.update_inline_completion(
                    &doc,
                    offset,
                    enable_inline_completion
                );
            });
        }

        let path2 = path.clone();
        let send = create_ext_action(
            self.scope,
            move |items: Vec<lsp_types::InlineCompletionItem>| {
                let items = doc.lines.with_untracked(|b| {
                    items
                        .into_iter()
                        .map(|item| InlineCompletionItem::from_lsp(b.buffer(), item))
                        .collect()
                });
                inline_completion.update(|c| {
                    c.set_items(items, offset, path2);
                    c.update_doc(&doc, offset);
                });
            }
        );

        inline_completion.update(|c| c.status = InlineCompletionStatus::Started);

        self.common.proxy.get_inline_completions(
            path,
            position,
            trigger_kind,
            move |(_, res)| {
                if let Ok(ProxyResponse::GetInlineCompletions {
                    completions: items
                }) = res
                {
                    let items = match items {
                        lsp_types::InlineCompletionResponse::Array(items) => items,
                        // Currently does not have any relevant extra fields
                        lsp_types::InlineCompletionResponse::List(items) => {
                            items.items
                        },
                    };
                    send(items);
                }
            }
        );
        Ok(())
    }

    /// Check if there are inline completions that are being rendered
    fn has_inline_completions(&self) -> bool {
        self.common.inline_completion.with_untracked(|completion| {
            completion.status != InlineCompletionStatus::Inactive
                && !completion.items.is_empty()
        })
    }

    pub fn select_completion(&self) {
        let item = self
            .common
            .completion
            .with_untracked(|c| c.current_item().cloned());
        self.cancel_completion();
        let doc = self.doc();
        if let Some(item) = item {
            if item.item.data.is_some() {
                let editor = self.clone();
                let rev = doc.lines.with_untracked(|b| b.buffer().rev());
                let path = doc.content.with_untracked(|c| c.path().cloned());
                let offset = self.cursor().with_untracked(|c| c.offset());
                let buffer = doc.lines.with_untracked(|b| b.signal_buffer_rev());
                let content = doc.content;
                let send = create_ext_action(self.scope, move |item| {
                    if editor.cursor().with_untracked(|c| c.offset() != offset) {
                        return;
                    }
                    if buffer.get_untracked() != rev
                        || content.with_untracked(|content| {
                            content.path() != path.as_ref()
                        })
                    {
                        return;
                    }
                    if let Err(err) = editor.apply_completion_item(&item) {
                        log::error!("{:?}", err);
                    }
                });
                self.common.proxy.completion_resolve(
                    item.plugin_id,
                    item.item.clone(),
                    move |(_, result)| {
                        let item =
                            if let Ok(ProxyResponse::CompletionResolveResponse {
                                item
                            }) = result
                            {
                                *item
                            } else {
                                item.item.clone()
                            };
                        send(item);
                    }
                );
            } else if let Err(err) = self.apply_completion_item(&item.item) {
                log::error!("{:?}", err);
            }
        }
    }

    pub fn cancel_completion(&self) {
        if self.common.completion.with_untracked(|c| c.status)
            == CompletionStatus::Inactive
        {
            return;
        }
        self.common.completion.update(|c| {
            c.cancel();
        });

        self.doc().clear_completion_lens()
    }

    /// Update the displayed autocompletion box
    /// Sends a request to the LSP for completion information
    fn update_completion(&self, display_if_empty_input: bool) {
        if self.get_mode() != Mode::Insert {
            self.cancel_completion();
            return;
        }

        let doc = self.doc();
        let path = match if doc.loaded() {
            doc.content.with_untracked(|c| c.path().cloned())
        } else {
            None
        } {
            Some(path) => path,
            None => return
        };

        let offset = self.cursor().with_untracked(|c| c.offset());
        let (start_offset, input, char) = doc.lines.with_untracked(|x| {
            let buffer = x.buffer();
            let start_offset = buffer.prev_code_boundary(offset);
            let end_offset = buffer.next_code_boundary(offset);
            let input = buffer.slice_to_cow(start_offset..end_offset).to_string();
            let char = if start_offset == 0 {
                "".to_string()
            } else {
                buffer
                    .slice_to_cow(start_offset - 1..start_offset)
                    .to_string()
            };
            (start_offset, input, char)
        });
        if !display_if_empty_input && input.is_empty() && char != "." && char != ":"
        {
            self.cancel_completion();
            return;
        }

        if self.common.completion.with_untracked(|completion| {
            completion.status != CompletionStatus::Inactive
                && completion.offset == start_offset
                && completion.path == path
        }) {
            self.common.completion.update(|completion| {
                completion.update_input(input.clone());

                if !completion.input_items.contains_key("") {
                    let start_pos = match doc.lines.with_untracked(|x| {
                        x.buffer().offset_to_position(start_offset)
                    }) {
                        Ok(rs) => rs,
                        Err(err) => {
                            error!("{err:?}");
                            return;
                        }
                    };
                    completion.request(
                        self.id(),
                        &self.common.proxy,
                        path.clone(),
                        "".to_string(),
                        start_pos
                    );
                }

                if !completion.input_items.contains_key(&input) {
                    let position = match doc
                        .lines
                        .with_untracked(|x| x.buffer().offset_to_position(offset))
                    {
                        Ok(rs) => rs,
                        Err(err) => {
                            error!("{err:?}");
                            return;
                        }
                    };
                    completion.request(
                        self.id(),
                        &self.common.proxy,
                        path,
                        input,
                        position
                    );
                }
            });
            let cursor_offset = self.cursor().with_untracked(|c| c.offset());
            self.common
                .completion
                .get_untracked()
                .update_document_completion(self, cursor_offset);

            return;
        }

        let doc = self.doc();
        self.common.completion.update(|completion| {
            completion.path.clone_from(&path);
            completion.offset = start_offset;
            completion.input.clone_from(&input);
            completion.status = CompletionStatus::Started;
            completion.input_items.clear();
            completion.request_id += 1;
            let start_pos = match doc
                .lines
                .with_untracked(|x| x.buffer().offset_to_position(start_offset))
            {
                Ok(rs) => rs,
                Err(err) => {
                    error!("{err:?}");
                    return;
                }
            };
            completion.request(
                self.id(),
                &self.common.proxy,
                path.clone(),
                "".to_string(),
                start_pos
            );

            if !input.is_empty() {
                let position = match doc
                    .lines
                    .with_untracked(|x| x.buffer().offset_to_position(offset))
                {
                    Ok(rs) => rs,
                    Err(err) => {
                        error!("{err:?}");
                        return;
                    }
                };
                completion.request(
                    self.id(),
                    &self.common.proxy,
                    path,
                    input,
                    position
                );
            }
        });
    }

    /// Check if there are completions that are being rendered
    fn has_completions(&self) -> bool {
        self.common.completion.with_untracked(|completion| {
            completion.status != CompletionStatus::Inactive
                && !completion.filtered_items.is_empty()
        })
    }

    fn apply_completion_item(&self, item: &CompletionItem) -> anyhow::Result<()> {
        log::debug!("apply_completion_item {:?}", item);
        let doc = self.doc();
        let buffer = doc.lines.with_untracked(|x| x.buffer().clone());
        let cursor = self.cursor().get_untracked();
        // Get all the edits which would be applied in places other than right where
        // the cursor is
        let mut additional_edit: Vec<_> = item
            .additional_text_edits
            .as_ref()
            .into_iter()
            .flatten()
            .map(|edit| {
                let selection = Selection::region(
                    buffer.offset_of_position(&edit.range.start)?,
                    buffer.offset_of_position(&edit.range.end)?
                );
                Ok((selection, edit.new_text.as_str()))
            })
            .filter_map(|x: Result<(Selection, &str)>| match x {
                Ok(rs) => Some(rs),
                Err(err) => {
                    error!("{err:?}");
                    None
                }
            })
            .collect::<Vec<(Selection, &str)>>();

        let text_format = item
            .insert_text_format
            .unwrap_or(lsp_types::InsertTextFormat::PLAIN_TEXT);
        if let Some(edit) = &item.text_edit {
            match edit {
                CompletionTextEdit::Edit(edit) => {
                    let offset = cursor.offset();
                    let start_offset = buffer.prev_code_boundary(offset);
                    let end_offset = buffer.next_code_boundary(offset);
                    let edit_start = buffer.offset_of_position(&edit.range.start)?;
                    let edit_end = buffer.offset_of_position(&edit.range.end)?;

                    let selection = Selection::region(
                        start_offset.min(edit_start),
                        end_offset.max(edit_end)
                    );
                    match text_format {
                        lsp_types::InsertTextFormat::PLAIN_TEXT => {
                            self.do_edit(
                                &selection,
                                &[
                                    &[(selection.clone(), edit.new_text.as_str())][..],
                                    &additional_edit[..],
                                ]
                                    .concat(), false,
                            );
                            return Ok(());
                        },

                        lsp_types::InsertTextFormat::SNIPPET => {
                            let snippet = Snippet::from_str(&edit.new_text)?;
                            let text = snippet.text();
                            additional_edit.push((selection.clone(), text.as_str()));
                            self.completion_apply_snippet(
                                snippet,
                                &selection,
                                additional_edit,
                                start_offset
                            )?;
                            return Ok(());
                        },
                        _ => {}
                    }
                },
                CompletionTextEdit::InsertAndReplace(_) => ()
            }
        }

        let offset = cursor.offset();
        let start_offset = buffer.prev_code_boundary(offset);
        let end_offset = buffer.next_code_boundary(offset);
        let selection = Selection::region(start_offset, end_offset);

        self.do_edit(
            &selection,
            &[
                &[(
                    selection.clone(),
                    item.insert_text.as_deref().unwrap_or(item.label.as_str())
                )][..],
                &additional_edit[..]
            ]
            .concat(),
            false
        );
        self.check_auto_save();
        Ok(())
    }

    pub fn completion_apply_snippet(
        &self,
        snippet: Snippet,
        selection: &Selection,
        additional_edit: Vec<(Selection, &str)>,
        start_offset: usize
    ) -> anyhow::Result<()> {
        // let snippet = Snippet::from_str(snippet)?;
        // let text = snippet.text();
        let mut cursor = self.cursor().get_untracked();
        let old_cursor = cursor.mode().clone();

        // additional_edit.push((selection.clone(), text.as_str()));
        let (b_text, delta, inval_lines) = self
            .doc()
            .do_raw_edit(&additional_edit, EditType::Completion)
            .ok_or_else(|| anyhow::anyhow!("not edited"))?;

        let selection = selection.apply_delta(&delta, true, InsertDrift::Default);

        let mut transformer = Transformer::new(&delta);
        let offset = transformer.transform(start_offset, false);
        let snippet_tabs = snippet.tabs(offset);

        let doc = self.doc();
        if snippet_tabs.is_empty() {
            doc.lines.update(|lines| {
                cursor.update_selection(lines.buffer(), selection);
                lines.set_cursor(old_cursor, cursor.mode().clone());
            });
            self.cursor().set(cursor);
            self.apply_deltas(&[(b_text, delta, inval_lines)]);
            return Ok(());
        }

        let mut selection = Selection::new();
        let (_tab, (start, end)) = &snippet_tabs[0];
        let region = SelRegion::new(*start, *end, None);
        selection.add_region(region);
        cursor.set_insert(selection);

        doc.lines.update(|lines| {
            lines.set_cursor(old_cursor, cursor.mode().clone());
        });
        self.cursor().set(cursor);
        self.apply_deltas(&[(b_text, delta, inval_lines)]);
        self.add_snippet_placeholders(snippet_tabs);
        Ok(())
    }

    fn add_snippet_placeholders(
        &self,
        new_placeholders: Vec<(usize, (usize, usize))>
    ) {
        self.snippet.update(|snippet| {
            if snippet.is_none() {
                if new_placeholders.len() > 1 {
                    *snippet = Some(new_placeholders);
                }
                return;
            }

            let placeholders = snippet.as_mut().unwrap();

            let mut current = 0;
            let offset = self.cursor().get_untracked().offset();
            for (i, (_, (start, end))) in placeholders.iter().enumerate() {
                if *start <= offset && offset <= *end {
                    current = i;
                    break;
                }
            }

            let v = placeholders.split_off(current);
            placeholders.extend_from_slice(&new_placeholders);
            placeholders.extend_from_slice(&v[1..]);
        });
    }

    fn check_auto_save(&self) {
        let autosave_interval = self
            .common
            .config
            .with_untracked(|config| config.editor.autosave_interval);
        if autosave_interval > 0 {
            if self.doc().content.with_untracked(|c| c.path().is_none()) {
                return;
            };
            let editor = self.clone();
            let rev = self.doc().rev();
            exec_after(Duration::from_millis(autosave_interval), move |_| {
                let is_pristine = editor
                    .doc()
                    .lines
                    .with_untracked(|x| x.buffer().is_pristine());
                let is_current_rec = editor.doc().rev() == rev;
                if !is_pristine && is_current_rec {
                    editor.save(true, || {});
                }
            });
        }
    }

    pub fn do_edit(
        &self,
        old_selection: &Selection,
        edits: &[(Selection, &str)],
        format_before_save: bool
    ) {
        // log::debug!("{:?} {}", old_selection, format_before_save);
        let mut cursor = self.cursor().get_untracked();
        let doc = self.doc();

        let rev_offset = if format_before_save {
            doc.lines.with_untracked(|x| {
                x.buffer()
                    .text()
                    .slice_to_cow(0..cursor.offset())
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .count()
            })
        } else {
            0
        };

        // let rev_offset = doc.buffer.with_untracked(|x| x.len()) - cursor.offset();
        let (text, delta, inval_lines) =
            match doc.do_raw_edit(edits, EditType::Completion) {
                Some(e) => e,
                None => return
            };
        let selection =
            old_selection.apply_delta(&delta, true, InsertDrift::Default);

        let old_cursor = cursor.mode().clone();
        doc.lines.update(|lines| {
            cursor.update_selection(lines.buffer(), selection);
            let rope = lines.buffer().text();
            if format_before_save {
                let offset = rope
                    .slice_to_cow(0..rope.len())
                    .chars()
                    .enumerate()
                    .filter(|(_, c)| !c.is_whitespace())
                    .nth(rev_offset.saturating_sub(1))
                    .map(|(index, _)| index + 1)
                    .unwrap_or_default();
                cursor.set_offset(offset, false, false);
            }
            lines.set_cursor(old_cursor, cursor.mode().clone());
        });
        self.cursor().set(cursor);

        self.apply_deltas(&[(text, delta, inval_lines)]);
    }

    pub fn do_text_edit(&self, edits: &[TextEdit], format_before_save: bool) {
        let (selection, edits) = self.doc().lines.with_untracked(|x| {
            let selection = self.cursor().get_untracked().edit_selection(x.buffer());
            let edits = edits
                .iter()
                .map(|edit| {
                    let selection = Selection::region(
                        x.buffer().offset_of_position(&edit.range.start)?,
                        x.buffer().offset_of_position(&edit.range.end)?
                    );
                    // log::debug!("{edit:?} {selection:?}");
                    Ok((selection, edit.new_text.as_str()))
                })
                .filter_map(|x: Result<(Selection, &str)>| match x {
                    Ok(rs) => Some(rs),
                    Err(err) => {
                        error!("{err:?}");
                        None
                    }
                })
                .collect::<Vec<_>>();
            (selection, edits)
        });
        let selection = match selection {
            Ok(rs) => rs,
            Err(err) => {
                error!("{err:?}");
                return;
            }
        };

        self.do_edit(&selection, &edits, format_before_save);
    }

    fn apply_deltas(&self, deltas: &[(Rope, RopeDelta, InvalLines)]) {
        if !deltas.is_empty() && !self.confirmed.get_untracked() {
            self.confirmed.set(true);
        }
        for (_, delta, _) in deltas {
            // self.inactive_apply_delta(delta);
            self.update_snippet_offset(delta);
            // self.update_breakpoints(delta);
        }
        // self.update_signature();
    }

    fn update_snippet_offset(&self, delta: &RopeDelta) {
        if self.snippet.with_untracked(|s| s.is_some()) {
            self.snippet.update(|snippet| {
                let mut transformer = Transformer::new(delta);
                *snippet = Some(
                    snippet
                        .as_ref()
                        .unwrap()
                        .iter()
                        .map(|(tab, (start, end))| {
                            (
                                *tab,
                                (
                                    transformer.transform(*start, false),
                                    transformer.transform(*end, true)
                                )
                            )
                        })
                        .collect()
                );
            });
        }
    }

    fn do_go_to_location(
        &self,
        location: EditorLocation,
        edits: Option<Vec<TextEdit>>
    ) {
        if let Some(position) = location.position {
            self.go_to_position(position, location.scroll_offset, edits);
        } else if let Some(edits) = edits.as_ref() {
            self.do_text_edit(edits, false);
        } else {
            let db: Arc<LapceDb> = use_context().unwrap();
            if let Ok(info) = db.get_doc_info(&self.common.workspace, &location.path)
            {
                self.go_to_position(
                    EditorPosition::Offset(info.cursor_offset),
                    Some(Vec2::new(info.scroll_offset.0, info.scroll_offset.1)),
                    edits
                );
            }
        }
    }

    pub fn go_to_location(
        &self,
        location: EditorLocation,
        new_doc: bool,
        edits: Option<Vec<TextEdit>>
    ) {
        if !new_doc {
            self.do_go_to_location(location, edits);
        } else {
            let loaded = self.doc().loaded;
            let editor = self.clone();
            self.scope.create_effect(move |prev_loaded| {
                if prev_loaded == Some(true) {
                    return true;
                }

                let loaded = loaded.get();
                if loaded {
                    editor.do_go_to_location(location.clone(), edits.clone());
                }
                loaded
            });
        }
    }

    pub fn go_to_position(
        &self,
        position: EditorPosition,
        scroll_offset: Option<Vec2>,
        edits: Option<Vec<TextEdit>>
    ) {
        let offset = match self
            .doc()
            .lines
            .with_untracked(|x| position.to_offset(x.buffer()))
        {
            Ok(rs) => rs,
            Err(err) => {
                error!("{err:?}");
                return;
            }
        };
        let modal = self
            .common
            .config
            .with_untracked(|config| config.core.modal);
        self.cursor().set(if modal {
            Cursor::new(CursorMode::Normal(offset), None, None)
        } else {
            Cursor::new(CursorMode::Insert(Selection::caret(offset)), None, None)
        });
        if let Some(scroll_offset) = scroll_offset {
            self.editor.scroll_to.set(Some(scroll_offset));
        }
        if let Some(edits) = edits.as_ref() {
            self.do_text_edit(edits, false);
        }
    }

    pub fn get_code_actions(&self) {
        let doc = self.doc();
        let path = match if doc.loaded() {
            doc.content.with_untracked(|c| c.path().cloned())
        } else {
            None
        } {
            Some(path) => path,
            None => return
        };

        let offset = self.cursor().with_untracked(|c| c.offset());
        let exists = doc
            .code_actions()
            .with_untracked(|c| c.contains_key(&offset));

        if exists {
            return;
        }

        // insert some empty data, so that we won't make the request again
        doc.code_actions().update(|c| {
            c.insert(offset, (PluginId(0), im::Vector::new()));
        });

        let (position, rev, diagnostics) = doc.lines.with_untracked(|buffer| {
            let buffer = buffer.buffer();
            let position = buffer.offset_to_position(offset);
            let rev = doc.rev();

            // Get the diagnostics for the current line, which the LSP might use to
            // inform what code actions are available (such as fixes for
            // the diagnostics).
            let diagnostics = doc
                .diagnostics()
                .diagnostics_span
                .get_untracked()
                .iter_chunks(offset..offset)
                .filter(|(iv, _diag)| iv.start <= offset && iv.end >= offset)
                .map(|(_iv, diag)| diag)
                .cloned()
                .collect();

            (position, rev, diagnostics)
        });
        let position = match position {
            Ok(rs) => rs,
            Err(err) => {
                error!("{err:?}");
                return;
            }
        };

        let send = create_ext_action(
            self.scope,
            move |resp: (PluginId, CodeActionResponse)| {
                if doc.rev() == rev {
                    doc.code_actions().update(|c| {
                        c.insert(offset, (resp.0, resp.1.into()));
                    });
                }
            }
        );

        self.common.proxy.get_code_actions(
            path,
            position,
            diagnostics,
            move |(_, result)| {
                if let Ok(ProxyResponse::GetCodeActionsResponse {
                    plugin_id,
                    resp
                }) = result
                {
                    send((plugin_id, resp))
                }
            }
        );
    }

    pub fn show_code_actions(&self, mouse_click: bool) {
        let offset = self.cursor().with_untracked(|c| c.offset());
        let doc = self.doc();
        let code_actions = doc
            .code_actions()
            .with_untracked(|c| c.get(&offset).cloned());
        if let Some((plugin_id, code_actions)) = code_actions {
            if !code_actions.is_empty() {
                self.common.internal_command.send(
                    InternalCommand::ShowCodeActions {
                        offset,
                        mouse_click,
                        plugin_id,
                        code_actions
                    }
                );
            }
        }
    }

    fn do_save(&self, after_action: impl FnOnce() + 'static) {
        self.doc().save(after_action);
    }

    pub fn save(
        &self,
        allow_formatting: bool,
        after_action: impl FnOnce() + 'static
    ) {
        let doc = self.doc();
        let is_pristine = doc.is_pristine();
        let content = doc.content.get_untracked();

        if let DocContent::Scratch { .. } = &content {
            self.common
                .internal_command
                .send(InternalCommand::SaveScratchDoc2 { doc });
            return;
        }

        if content.path().is_some() && is_pristine {
            return;
        }

        let (normalize_line_endings, format_on_save) =
            self.common.config.with_untracked(|config| {
                (
                    config.editor.normalize_line_endings,
                    config.editor.format_on_save
                )
            });

        let DocContent::File { path, .. } = content else {
            return;
        };

        // If we are disallowing formatting (such as due to a manual save without
        // formatting), then we skip normalizing line endings as a common
        // reason for that is large files. (but if the save is typical, even
        // if config format_on_save is false, we normalize)
        if allow_formatting && normalize_line_endings {
            if let Err(err) =
                self.run_edit_command(&EditCommand::NormalizeLineEndings)
            {
                error!("{:?}", err);
            }
        }

        let rev = doc.rev();
        let format_on_save = allow_formatting && format_on_save;
        if format_on_save {
            let editor = self.clone();
            let send = create_ext_action(self.scope, move |result| {
                if let Ok(ProxyResponse::GetDocumentFormatting { edits }) = result {
                    let current_rev = editor.doc().rev();
                    if current_rev == rev {
                        // log::debug!("{:?}", edits);
                        editor.do_text_edit(&edits, true);
                    }
                }
                editor.do_save(after_action);
            });

            let proxy = self.common.proxy.clone();
            proxy.get_document_formatting(path, move |(_, result)| {
                send(result);
            });
        } else {
            self.do_save(after_action);
        }
    }

    pub fn format(&self) {
        let doc = self.doc();
        let rev = doc.rev();
        let content = doc.content.get_untracked();

        if let DocContent::File { path, .. } = content {
            let editor = self.clone();
            let send = create_ext_action(self.scope, move |result| {
                if let Ok(ProxyResponse::GetDocumentFormatting { edits }) = result {
                    let current_rev = editor.doc().rev();
                    if current_rev == rev {
                        editor.do_text_edit(&edits, true);
                    }
                }
            });

            let proxy = self.common.proxy.clone();
            proxy.get_document_formatting(path, move |(_, result)| {
                send(result);
            });
        }
    }

    fn search_whole_word_forward(&self, mods: Modifiers) {
        let offset = self.cursor().with_untracked(|c| c.offset());
        let (word, buffer) = self.doc().lines.with_untracked(|buffer| {
            let buffer = buffer.buffer();
            let (start, end) = buffer.select_word(offset);
            (buffer.slice_to_cow(start..end).to_string(), buffer.clone())
        });
        self.common.internal_command.send(InternalCommand::Search {
            pattern: Some(word)
        });
        let next = self.common.find.next(buffer.text(), offset, false, true);

        if let Some((start, _end)) = next {
            self.run_move_command(&Movement::Offset(start), None, mods);
        }
    }

    fn search_forward(&self, mods: Modifiers) {
        let offset = self.cursor().with_untracked(|c| c.offset());
        let text = self
            .doc()
            .lines
            .with_untracked(|buffer| buffer.buffer().text().clone());
        let next = self.common.find.next(&text, offset, false, true);

        if let Some((start, _end)) = next {
            self.run_move_command(&Movement::Offset(start), None, mods);
        }
    }

    fn search_backward(&self, mods: Modifiers) {
        let offset = self.cursor().with_untracked(|c| c.offset());
        let text = self
            .doc()
            .lines
            .with_untracked(|buffer| buffer.buffer().text().clone());
        let next = self.common.find.next(&text, offset, true, true);

        if let Some((start, _end)) = next {
            self.run_move_command(&Movement::Offset(start), None, mods);
        }
    }

    fn replace_next(&self, text: &str) {
        let offset = self.cursor().with_untracked(|c| c.offset());
        let buffer = self
            .doc()
            .lines
            .with_untracked(|buffer| buffer.buffer().clone());
        let next = self.common.find.next(buffer.text(), offset, false, true);

        if let Some((start, end)) = next {
            let selection = Selection::region(start, end);
            self.do_edit(&selection, &[(selection.clone(), text)], false);
        }
    }

    fn replace_all(&self, text: &str) {
        let offset = self.cursor().with_untracked(|c| c.offset());

        self.doc().update_find();

        let edits: Vec<(Selection, &str)> = self
            .doc()
            .find_result
            .occurrences
            .get_untracked()
            .regions()
            .iter()
            .map(|region| (Selection::region(region.start, region.end), text))
            .collect();
        if !edits.is_empty() {
            self.do_edit(&Selection::caret(offset), &edits, false);
        }
    }

    pub fn save_doc_position(&self) {
        let doc = self.doc();
        let path = match if doc.loaded() {
            doc.content.with_untracked(|c| c.path().cloned())
        } else {
            None
        } {
            Some(path) => path,
            None => return
        };

        let cursor_offset = self.cursor().with_untracked(|c| c.offset());
        let scroll_offset = self.viewport().origin().to_vec2();

        let db: Arc<LapceDb> = use_context().unwrap();
        db.save_doc_position(
            &self.common.workspace,
            path,
            cursor_offset,
            scroll_offset,
            &self.common.local_task
        );
    }

    fn rename(&self) -> Result<()> {
        let doc = self.doc();
        let path = match if doc.loaded() {
            doc.content.with_untracked(|c| c.path().cloned())
        } else {
            None
        } {
            Some(path) => path,
            None => return Ok(())
        };

        let offset = self.cursor().with_untracked(|c| c.offset());
        let (position, rev) = doc.lines.with_untracked(|buffer| {
            (buffer.buffer().offset_to_position(offset), doc.rev())
        });
        let position = position?;
        let cursor = self.cursor();
        let buffer = doc.lines.with_untracked(|x| x.signal_buffer());
        let internal_command = self.common.internal_command;
        let local_path = path.clone();
        let send = create_ext_action(self.scope, move |result| {
            if let Ok(ProxyResponse::PrepareRename { resp }) = result {
                if buffer.with_untracked(|buffer| buffer.rev()) != rev {
                    return;
                }

                if cursor.with_untracked(|c| c.offset()) != offset {
                    return;
                }

                let (start, _end, position, placeholder) = match buffer
                    .with_untracked(|buffer| match resp {
                        lsp_types::PrepareRenameResponse::Range(range) => {
                            Ok::<
                                (
                                    usize,
                                    usize,
                                    lsp_types::Position,
                                    std::option::Option<std::string::String>
                                ),
                                anyhow::Error
                            >((
                                buffer.offset_of_position(&range.start)?,
                                buffer.offset_of_position(&range.end)?,
                                range.start,
                                None
                            ))
                        },
                        lsp_types::PrepareRenameResponse::RangeWithPlaceholder {
                            range,
                            placeholder
                        } => Ok((
                            buffer.offset_of_position(&range.start)?,
                            buffer.offset_of_position(&range.end)?,
                            range.start,
                            Some(placeholder)
                        )),
                        lsp_types::PrepareRenameResponse::DefaultBehavior {
                            ..
                        } => {
                            let start = buffer.prev_code_boundary(offset);
                            let position = buffer.offset_to_position(start)?;
                            Ok((
                                start,
                                buffer.next_code_boundary(offset),
                                position,
                                None
                            ))
                        }
                    }) {
                    Ok(rs) => rs,
                    Err(err) => {
                        error!("{err:?}");
                        return;
                    }
                };
                let placeholder = placeholder.unwrap_or_else(|| {
                    buffer.with_untracked(|buffer| {
                        let (start, end) = buffer.select_word(offset);
                        buffer.slice_to_cow(start..end).to_string()
                    })
                });
                internal_command.send(InternalCommand::StartRename {
                    path: local_path.clone(),
                    placeholder,
                    start,
                    position
                });
            }
        });
        self.common
            .proxy
            .prepare_rename(path, position, move |(_, result)| {
                send(result);
            });
        Ok(())
    }

    pub fn word_at_cursor(&self) -> String {
        let doc = self.doc();
        let region = self.cursor().with_untracked(|c| match &c.mode() {
            CursorMode::Normal(offset) => SelRegion::caret(*offset),
            CursorMode::Visual {
                start,
                end,
                mode: _
            } => SelRegion::new(
                *start.min(end),
                doc.lines.with_untracked(|buffer| {
                    buffer.buffer().next_grapheme_offset(
                        *start.max(end),
                        1,
                        buffer.buffer().len()
                    )
                }),
                None
            ),
            CursorMode::Insert(selection) => *selection.last_inserted().unwrap()
        });

        if region.is_caret() {
            doc.lines.with_untracked(|buffer| {
                let (start, end) = buffer.buffer().select_word(region.start);
                buffer.buffer().slice_to_cow(start..end).to_string()
            })
        } else {
            doc.lines.with_untracked(|buffer| {
                buffer
                    .buffer()
                    .slice_to_cow(region.min()..region.max())
                    .to_string()
            })
        }
    }

    pub fn selected_word(&self) -> Option<String> {
        let doc = self.doc();
        let region = self.cursor().with_untracked(|c| match &c.mode() {
            CursorMode::Normal(offset) => SelRegion::caret(*offset),
            CursorMode::Visual {
                start,
                end,
                mode: _
            } => SelRegion::new(
                *start.min(end),
                doc.lines.with_untracked(|buffer| {
                    buffer.buffer().next_grapheme_offset(
                        *start.max(end),
                        1,
                        buffer.buffer().len()
                    )
                }),
                None
            ),
            CursorMode::Insert(selection) => *selection.last_inserted().unwrap()
        });

        if !region.is_caret() {
            Some(doc.lines.with_untracked(|buffer| {
                buffer
                    .buffer()
                    .slice_to_cow(region.min()..region.max())
                    .to_string()
            }))
        } else {
            None
        }
    }

    pub fn clear_search(&self) {
        self.common.find.visual.set(false);
        self.find_focus.set(false);
    }

    fn search(&self) {
        if self.common.find.visual.get_untracked() {
            self.clear_search();
        } else {
            let pattern = self.selected_word();
            self.common
                .internal_command
                .send(InternalCommand::Search { pattern });
            self.common.find.visual.set(true);
            self.find_focus.set(true);
            self.common.find.replace_focus.set(false);
        }
    }

    pub fn pointer_down(&self, pointer_event: &PointerInputEvent) {
        self.cancel_completion();
        self.cancel_inline_completion();
        if let Some(editor_tab_id) = self.editor_tab_id.get_untracked() {
            self.common
                .internal_command
                .send(InternalCommand::FocusEditorTab { editor_tab_id });
        }
        if self
            .doc()
            .content
            .with_untracked(|content| !content.is_local())
        {
            self.common.focus.set(Focus::Workbench);
            self.find_focus.set(false);
        }
        match pointer_event.button {
            PointerButton::Mouse(MouseButton::Primary) => {
                self.active().set(true);
                self.left_click(pointer_event);

                let y = pointer_event.pos.y - self.editor.viewport().y0;
                if self.sticky_header_height.get_untracked() > y {
                    let index = y as usize
                        / self
                            .common
                            .config
                            .with_untracked(|config| config.editor.line_height());
                    if let (Some(path), Some(line)) = (
                        self.doc().content.get_untracked().path(),
                        self.sticky_header_info
                            .get_untracked()
                            .sticky_lines
                            .get(index)
                    ) {
                        self.common.internal_command.send(
                            InternalCommand::JumpToLocation {
                                location: EditorLocation {
                                    path:               path.clone(),
                                    position:           Some(EditorPosition::Line(
                                        *line
                                    )),
                                    scroll_offset:      None,
                                    ignore_unconfirmed: true,
                                    same_editor_tab:    false
                                }
                            }
                        );
                        return;
                    }
                }

                let control = (cfg!(target_os = "macos")
                    && pointer_event.modifiers.meta())
                    || (cfg!(not(target_os = "macos"))
                        && pointer_event.modifiers.control());
                if let Some(rs) = self.result_of_left_click(pointer_event.pos) {
                    match rs {
                        ClickResult::NoHint => {
                            if control {
                                self.common.lapce_command.send(LapceCommand {
                                    kind: CommandKind::Focus(
                                        FocusCommand::GotoDefinition
                                    ),
                                    data: None
                                })
                            }
                        },
                        ClickResult::MatchWithoutLocation
                        | ClickResult::MatchFolded => {},
                        ClickResult::MatchHint(location) => {
                            if control {
                                let Ok(path) = location.uri.to_file_path() else {
                                    return;
                                };
                                self.common.internal_command.send(
                                    InternalCommand::JumpToLocation {
                                        location: EditorLocation {
                                            path,
                                            position: Some(
                                                EditorPosition::Position(
                                                    location.range.start
                                                )
                                            ),
                                            scroll_offset: None,
                                            ignore_unconfirmed: true,
                                            same_editor_tab: false
                                        }
                                    }
                                );
                            }
                        },
                    }
                }
            },
            PointerButton::Mouse(MouseButton::Secondary) => {
                self.right_click(pointer_event);
            },
            _ => {}
        }
    }

    fn result_of_left_click(&self, pos: Point) -> Option<ClickResult> {
        self.doc()
            .lines
            .try_update(|x| match x.result_of_left_click(pos) {
                Ok(rs) => rs,
                Err(err) => {
                    error!("{err:?}");
                    ClickResult::NoHint
                }
            })
    }

    fn left_click(&self, pointer_event: &PointerInputEvent) {
        match pointer_event.count {
            1 => {
                self.single_click(pointer_event);
            },
            2 => {
                self.double_click(pointer_event);
            },
            3 => {
                self.triple_click(pointer_event);
            },
            _ => {}
        }
    }

    fn single_click(&self, pointer_event: &PointerInputEvent) {
        self.editor.single_click(pointer_event, &self.common);
    }

    fn double_click(&self, pointer_event: &PointerInputEvent) {
        self.editor.double_click(pointer_event);
    }

    fn triple_click(&self, pointer_event: &PointerInputEvent) {
        self.editor.triple_click(pointer_event);
    }

    pub fn pointer_move(&self, pointer_event: &PointerMoveEvent) {
        let mode = self.cursor().with_untracked(|c| c.mode().clone());
        let (offset, is_inside) =
            match self.editor.offset_of_point(&mode, pointer_event.pos) {
                Ok(rs) => rs,
                Err(err) => {
                    error!("{err:?}");
                    return;
                }
            };
        if self.active().get_untracked()
            && self.cursor().with_untracked(|c| c.offset()) != offset
        {
            self.cursor().update(|cursor| {
                cursor.set_offset(offset, true, pointer_event.modifiers.alt())
            });
        }
        if self.common.hover.active.get_untracked() {
            let hover_editor_id = self.common.hover.editor_id.get_untracked();
            if hover_editor_id != self.id() {
                self.common.hover.active.set(false);
            } else {
                let current_offset = self.common.hover.offset.get_untracked();
                let start_offset = self.doc().lines.with_untracked(|buffer| {
                    buffer.buffer().next_code_boundary(offset)
                });
                if current_offset != start_offset {
                    self.common.hover.active.set(false);
                }
            }
        }
        let hover_delay =
            self.common.config.with_untracked(|x| x.editor.hover_delay);
        if hover_delay > 0 {
            if is_inside {
                let editor = self.clone();
                let mouse_hover_timer = self.common.mouse_hover_timer;
                let timer_token =
                    exec_after(Duration::from_millis(hover_delay), move |token| {
                        if mouse_hover_timer.try_get_untracked() == Some(token)
                            && editor.editor_tab_id.try_get_untracked().is_some()
                        {
                            let end_offset =
                                editor.doc().lines.with_untracked(|buffer| {
                                    buffer.buffer().next_code_boundary(offset)
                                });
                            editor.update_hover(end_offset);
                        }
                    });
                mouse_hover_timer.set(timer_token);
            } else {
                self.common.mouse_hover_timer.set(TimerToken::INVALID);
            }
        }
    }

    pub fn pointer_up(&self, pointer_event: &PointerInputEvent) {
        self.editor.pointer_up(pointer_event);
    }

    pub fn pointer_leave(&self) {
        self.common.mouse_hover_timer.set(TimerToken::INVALID);
    }

    fn right_click(&self, pointer_event: &PointerInputEvent) {
        let mode = self.cursor().with_untracked(|c| c.mode().clone());
        let (offset, _) = match self.editor.offset_of_point(&mode, pointer_event.pos)
        {
            Ok(rs) => rs,
            Err(err) => {
                error!("{err:?}");
                return;
            }
        };
        let doc = self.doc();
        let pointer_inside_selection = doc.lines.with_untracked(|buffer| {
            self.cursor().with_untracked(|c| {
                match c.edit_selection(buffer.buffer()) {
                    Ok(rs) => rs,
                    Err(err) => {
                        error!("{err:?}");
                        return false;
                    }
                }
                .contains(offset)
            })
        });
        if !pointer_inside_selection {
            // move cursor to pointer position if outside current selection
            self.single_click(pointer_event);
        }

        let (path, is_file) = doc.content.with_untracked(|content| match content {
            DocContent::File { path, .. } => {
                (Some(path.to_path_buf()), path.is_file())
            },
            DocContent::Local
            | DocContent::History(_)
            | DocContent::Scratch { .. } => (None, false)
        });
        let mut menu = Menu::new("");
        let mut cmds = if is_file {
            if path
                .as_ref()
                .and_then(|x| x.file_name().and_then(|x| x.to_str()))
                .map(|x| x == "run.toml")
                .unwrap_or_default()
            {
                vec![
                    Some(CommandKind::Workbench(
                        LapceWorkbenchCommand::RevealInPanel
                    )),
                    Some(CommandKind::Workbench(
                        LapceWorkbenchCommand::RevealInFileExplorer
                    )),
                    Some(CommandKind::Workbench(
                        LapceWorkbenchCommand::SourceControlOpenActiveFileRemoteUrl
                    )),
                    None,
                    Some(CommandKind::Edit(EditCommand::ClipboardCut)),
                    Some(CommandKind::Edit(EditCommand::ClipboardCopy)),
                    Some(CommandKind::Edit(EditCommand::ClipboardPaste)),
                    Some(CommandKind::Workbench(
                        LapceWorkbenchCommand::AddRunDebugConfig
                    )),
                    None,
                    Some(CommandKind::Workbench(
                        LapceWorkbenchCommand::PaletteCommand
                    )),
                ]
            } else {
                vec![
                    Some(CommandKind::Focus(FocusCommand::GotoDefinition)),
                    Some(CommandKind::Focus(FocusCommand::GotoTypeDefinition)),
                    Some(CommandKind::Workbench(
                        LapceWorkbenchCommand::ShowCallHierarchy
                    )),
                    Some(CommandKind::Workbench(
                        LapceWorkbenchCommand::FindReferences
                    )),
                    Some(CommandKind::Workbench(
                        LapceWorkbenchCommand::GoToImplementation
                    )),
                    Some(CommandKind::Focus(FocusCommand::Rename)),
                    Some(CommandKind::Workbench(
                        LapceWorkbenchCommand::RunInTerminal
                    )),
                    None,
                    Some(CommandKind::Workbench(
                        LapceWorkbenchCommand::RevealInPanel
                    )),
                    Some(CommandKind::Workbench(
                        LapceWorkbenchCommand::RevealInFileExplorer
                    )),
                    Some(CommandKind::Workbench(
                        LapceWorkbenchCommand::RevealInDocumentSymbolPanel
                    )),
                    Some(CommandKind::Workbench(
                        LapceWorkbenchCommand::SourceControlOpenActiveFileRemoteUrl
                    )),
                    None,
                    Some(CommandKind::Edit(EditCommand::ClipboardCut)),
                    Some(CommandKind::Edit(EditCommand::ClipboardCopy)),
                    Some(CommandKind::Edit(EditCommand::ClipboardPaste)),
                    None,
                    Some(CommandKind::Workbench(
                        LapceWorkbenchCommand::PaletteCommand
                    )),
                ]
            }
        } else {
            vec![
                Some(CommandKind::Edit(EditCommand::ClipboardCut)),
                Some(CommandKind::Edit(EditCommand::ClipboardCopy)),
                Some(CommandKind::Edit(EditCommand::ClipboardPaste)),
                None,
                Some(CommandKind::Workbench(
                    LapceWorkbenchCommand::PaletteCommand
                )),
            ]
        };
        if self.diff_editor_id.get_untracked().is_some() && is_file {
            cmds.push(Some(CommandKind::Workbench(
                LapceWorkbenchCommand::GoToLocation
            )));
        }
        let lapce_command = self.common.lapce_command;
        for cmd in cmds {
            if let Some(cmd) = cmd {
                menu = menu.entry(
                    MenuItem::new(cmd.desc().unwrap_or_else(|| cmd.str())).action(
                        move || {
                            lapce_command.send(LapceCommand {
                                kind: cmd.clone(),
                                data: None
                            })
                        }
                    )
                );
            } else {
                menu = menu.separator();
            }
        }
        show_context_menu(menu, None);
    }

    fn update_hover(&self, offset: usize) {
        let doc = self.doc();
        let path = doc
            .content
            .with_untracked(|content| content.path().cloned());
        let position = match doc
            .lines
            .with_untracked(|buffer| buffer.buffer().offset_to_position(offset))
        {
            Ok(rs) => rs,
            Err(err) => {
                error!("{err:?}");
                return;
            }
        };
        let path = match path {
            Some(path) => path,
            None => return
        };
        let config = self.common.config;
        let hover_data = self.common.hover.clone();
        let editor_id = self.id();
        let directory = self.common.directory.clone();
        let send = create_ext_action(self.scope, move |resp| {
            if let Ok(ProxyResponse::HoverResponse { hover, .. }) = resp {
                let (
                    font_family,
                    editor_fg,
                    style_colors,
                    font_size,
                    markdown_blockquote,
                    editor_link
                ) = config.with_untracked(|config| {
                    (
                        config.editor.font_family.clone(),
                        config.color(LapceColor::EDITOR_FOREGROUND),
                        config.style_colors(),
                        config.ui.font_size() as f32,
                        config.color(LapceColor::MARKDOWN_BLOCKQUOTE),
                        config.color(LapceColor::EDITOR_LINK)
                    )
                });
                let content = parse_hover_resp(
                    hover,
                    &directory,
                    &font_family,
                    editor_fg,
                    &style_colors,
                    font_size,
                    markdown_blockquote,
                    editor_link
                );
                batch(|| {
                    hover_data.content.set(content);
                    hover_data.offset.set(offset);
                    hover_data.editor_id.set(editor_id);
                    hover_data.active.set(true);
                });
            }
        });
        self.common.proxy.get_hover(0, path, position, |(_, resp)| {
            send(resp);
        });
    }

    // reset the doc inside and move cursor back
    pub fn reset(&self) {
        let doc = self.doc();
        doc.reload(Rope::from(""), true);
        self.cursor()
            .update(|cursor| cursor.set_offset(0, false, false));
    }

    pub fn visual_line(&self, line: usize) -> usize {
        self.kind().with_untracked(|kind| match kind {
            EditorViewKind::Normal => line,
            EditorViewKind::Diff(diff) => {
                let is_right = diff.is_right;
                let mut last_change: Option<&DiffLines> = None;
                let mut visual_line = 0;
                let mut changes = diff.changes.iter().peekable();
                while let Some(change) = changes.next() {
                    match (is_right, change) {
                        (true, DiffLines::Left(range)) => {
                            if let Some(DiffLines::Right(_)) = changes.peek() {
                            } else {
                                visual_line += range.len();
                            }
                        },
                        (false, DiffLines::Right(range)) => {
                            let len = if let Some(DiffLines::Left(r)) = last_change {
                                range.len() - r.len().min(range.len())
                            } else {
                                range.len()
                            };
                            if len > 0 {
                                visual_line += len;
                            }
                        },
                        (true, DiffLines::Right(range))
                        | (false, DiffLines::Left(range)) => {
                            if line < range.end {
                                return visual_line + line - range.start;
                            }
                            visual_line += range.len();
                            if is_right {
                                if let Some(DiffLines::Left(r)) = last_change {
                                    let len = r.len() - r.len().min(range.len());
                                    if len > 0 {
                                        visual_line += len;
                                    }
                                }
                            }
                        },
                        (_, DiffLines::Both(info)) => {
                            let end = if is_right {
                                info.right.end
                            } else {
                                info.left.end
                            };
                            if line >= end {
                                visual_line += info.right.len()
                                    - info
                                        .skip
                                        .as_ref()
                                        .map(|skip| skip.len().saturating_sub(1))
                                        .unwrap_or(0);
                                last_change = Some(change);
                                continue;
                            }

                            let start = if is_right {
                                info.right.start
                            } else {
                                info.left.start
                            };
                            if let Some(skip) = info.skip.as_ref() {
                                if start + skip.start > line {
                                    return visual_line + line - start;
                                } else if start + skip.end > line {
                                    return visual_line + skip.start;
                                } else {
                                    return visual_line
                                        + (line - start - skip.len() + 1);
                                }
                            } else {
                                return visual_line + line - start;
                            }
                        }
                    }
                    last_change = Some(change);
                }
                visual_line
            }
        })
    }

    pub fn actual_line(&self, visual_line: usize, bottom_affinity: bool) -> usize {
        self.kind().with_untracked(|kind| match kind {
            EditorViewKind::Normal => visual_line,
            EditorViewKind::Diff(diff) => {
                let is_right = diff.is_right;
                let mut actual_line: usize = 0;
                let mut current_visual_line = 0;
                let mut last_change: Option<&DiffLines> = None;
                let mut changes = diff.changes.iter().peekable();
                while let Some(change) = changes.next() {
                    match (is_right, change) {
                        (true, DiffLines::Left(range)) => {
                            if let Some(DiffLines::Right(_)) = changes.peek() {
                            } else {
                                current_visual_line += range.len();
                                if current_visual_line >= visual_line {
                                    return if bottom_affinity {
                                        actual_line
                                    } else {
                                        actual_line.saturating_sub(1)
                                    };
                                }
                            }
                        },
                        (false, DiffLines::Right(range)) => {
                            let len = if let Some(DiffLines::Left(r)) = last_change {
                                range.len() - r.len().min(range.len())
                            } else {
                                range.len()
                            };
                            if len > 0 {
                                current_visual_line += len;
                                if current_visual_line >= visual_line {
                                    return actual_line;
                                }
                            }
                        },
                        (true, DiffLines::Right(range))
                        | (false, DiffLines::Left(range)) => {
                            let len = range.len();
                            if current_visual_line + len > visual_line {
                                return range.start
                                    + (visual_line - current_visual_line);
                            }
                            current_visual_line += len;
                            actual_line += len;
                            if is_right {
                                if let Some(DiffLines::Left(r)) = last_change {
                                    let len = r.len() - r.len().min(range.len());
                                    if len > 0 {
                                        current_visual_line += len;
                                        if current_visual_line > visual_line {
                                            return if bottom_affinity {
                                                actual_line
                                            } else {
                                                actual_line - range.len()
                                            };
                                        }
                                    }
                                }
                            }
                        },
                        (_, DiffLines::Both(info)) => {
                            let len = info.right.len();
                            let start = if is_right {
                                info.right.start
                            } else {
                                info.left.start
                            };

                            if let Some(skip) = info.skip.as_ref() {
                                if current_visual_line + skip.start == visual_line {
                                    return if bottom_affinity {
                                        actual_line + skip.end
                                    } else {
                                        (actual_line + skip.start).saturating_sub(1)
                                    };
                                } else if current_visual_line + skip.start + 1
                                    > visual_line
                                {
                                    return actual_line + visual_line
                                        - current_visual_line;
                                } else if current_visual_line + len - skip.len() + 1
                                    >= visual_line
                                {
                                    return actual_line
                                        + skip.end
                                        + (visual_line
                                            - current_visual_line
                                            - skip.start
                                            - 1);
                                }
                                actual_line += len;
                                current_visual_line += len - skip.len() + 1;
                            } else {
                                if current_visual_line + len > visual_line {
                                    return start
                                        + (visual_line - current_visual_line);
                                }
                                current_visual_line += len;
                                actual_line += len;
                            }
                        }
                    }
                    last_change = Some(change);
                }
                actual_line
            }
        })
    }
}

impl KeyPressFocus for EditorData {
    fn get_mode(&self) -> Mode {
        if self.common.find.visual.get_untracked() && self.find_focus.get_untracked()
        {
            Mode::Insert
        } else {
            self.cursor().with_untracked(|c| c.mode().simply_mode())
        }
    }

    fn check_condition(&self, condition: Condition) -> bool {
        match condition {
            Condition::InputFocus => {
                self.common.find.visual.get_untracked()
                    && self.find_focus.get_untracked()
            },
            Condition::ListFocus => self.has_completions(),
            Condition::CompletionFocus => self.has_completions(),
            Condition::InlineCompletionVisible => self.has_inline_completions(),
            Condition::OnScreenFindActive => {
                self.on_screen_find.with_untracked(|f| f.active)
            },
            Condition::InSnippet => self.snippet.with_untracked(|s| s.is_some()),
            Condition::EditorFocus => self
                .doc()
                .content
                .with_untracked(|content| !content.is_local()),
            Condition::SearchFocus => {
                self.common.find.visual.get_untracked()
                    && self.find_focus.get_untracked()
                    && !self.common.find.replace_focus.get_untracked()
            },
            Condition::ReplaceFocus => {
                self.common.find.visual.get_untracked()
                    && self.find_focus.get_untracked()
                    && self.common.find.replace_focus.get_untracked()
            },
            Condition::SearchActive => {
                if self.common.config.with_untracked(|x| x.core.modal)
                    && self.cursor().with_untracked(|c| !c.is_normal())
                {
                    false
                } else {
                    self.common.find.visual.get_untracked()
                }
            },
            _ => false
        }
    }

    fn run_command(
        &self,
        command: &crate::command::LapceCommand,
        count: Option<usize>,
        mods: Modifiers
    ) -> CommandExecuted {
        if self.common.find.visual.get_untracked() && self.find_focus.get_untracked()
        {
            match &command.kind {
                CommandKind::Edit(_)
                | CommandKind::Move(_)
                | CommandKind::MultiSelection(_) => {
                    if self.common.find.replace_focus.get_untracked() {
                        self.common.internal_command.send(
                            InternalCommand::ReplaceEditorCommand {
                                command: command.clone(),
                                count,
                                mods
                            }
                        );
                    } else {
                        self.common.internal_command.send(
                            InternalCommand::FindEditorCommand {
                                command: command.clone(),
                                count,
                                mods
                            }
                        );
                    }
                    return CommandExecuted::Yes;
                },
                _ => {}
            }
        }

        match &command.kind {
            crate::command::CommandKind::Workbench(_) => CommandExecuted::No,
            crate::command::CommandKind::Edit(cmd) => {
                match self.run_edit_command(cmd) {
                    Ok(rs) => rs,
                    Err(err) => {
                        error!("{err:?}");
                        CommandExecuted::No
                    }
                }
            },
            crate::command::CommandKind::Move(cmd) => {
                let movement = cmd.to_movement(count);
                self.run_move_command(&movement, count, mods)
            },
            crate::command::CommandKind::Scroll(cmd) => {
                if self
                    .doc()
                    .content
                    .with_untracked(|content| content.is_local())
                {
                    return CommandExecuted::No;
                }
                self.run_scroll_command(cmd, count, mods)
            },
            crate::command::CommandKind::Focus(cmd) => {
                if self
                    .doc()
                    .content
                    .with_untracked(|content| content.is_local())
                {
                    return CommandExecuted::No;
                }
                self.run_focus_command(cmd, count, mods)
            },
            crate::command::CommandKind::MotionMode(cmd) => {
                self.run_motion_mode_command(cmd, count)
            },
            crate::command::CommandKind::MultiSelection(cmd) => {
                self.run_multi_selection_command(cmd)
            },
        }
    }

    fn expect_char(&self) -> bool {
        if self.common.find.visual.get_untracked() && self.find_focus.get_untracked()
        {
            false
        } else {
            self.inline_find.with_untracked(|f| f.is_some())
                || self.on_screen_find.with_untracked(|f| f.active)
        }
    }

    fn receive_char(&self, c: &str) {
        if self.common.find.visual.get_untracked() && self.find_focus.get_untracked()
        {
            // find/relace editor receive char
            if self.common.find.replace_focus.get_untracked() {
                self.common.internal_command.send(
                    InternalCommand::ReplaceEditorReceiveChar { s: c.to_string() }
                );
                // todo 搜索框应该直接由键盘输入，而不是这样迂回
                // } else {
                //     self.common.internal_command.send(
                //         InternalCommand::FindEditorReceiveChar { s:
                // c.to_string() }     );
            }
        } else {
            self.common.hover.active.set(false);
            // normal editor receive char
            if self.get_mode() == Mode::Insert {
                let mut cursor = self.cursor().get_untracked();
                let deltas = self.doc().do_insert(&mut cursor, c);
                self.cursor().set(cursor);

                if !c
                    .chars()
                    .all(|c| c.is_whitespace() || c.is_ascii_whitespace())
                {
                    self.update_completion(false);
                } else {
                    self.cancel_completion();
                }

                if let Err(err) = self
                    .update_inline_completion(InlineCompletionTriggerKind::Automatic)
                {
                    error!("{:?}", err);
                }

                self.apply_deltas(&deltas);
                self.check_auto_save();
            } else if let Some(direction) = self.inline_find.get_untracked() {
                if let Err(err) = self.inline_find(direction.clone(), c) {
                    error!("{:?}", err);
                }
                self.last_inline_find.set(Some((direction, c.to_string())));
                self.inline_find.set(None);
            } else if self.on_screen_find.with_untracked(|f| f.active) {
                self.on_screen_find.update(|find| {
                    let pattern = format!("{}{c}", find.pattern);
                    find.regions = self.on_screen_find(&pattern);
                    find.pattern = pattern;
                });
            }
        }
    }
}

/// Custom signal wrapper for [`Doc`], because [`Editor`] only knows it as a
/// `Rc<dyn Document>`, and there is currently no way to have an
/// `RwSignal<Rc<Doc>>` and an `RwSignal<Rc<dyn Document>>`.
/// This could possibly be swapped with a generic impl?
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DocSignal {
    // TODO: replace with ReadSignal once that impls `track`
    inner: RwSignal<Rc<Doc>>
}

impl DocSignal {
    pub fn get(&self) -> Rc<Doc> {
        self.inner.get()
    }

    pub fn get_untracked(&self) -> Rc<Doc> {
        self.inner.get_untracked()
    }

    pub fn with<O>(&self, f: impl FnOnce(&Rc<Doc>) -> O) -> O {
        self.inner.with(|doc| {
            let doc = doc.clone();
            f(&doc)
        })
    }

    pub fn with_untracked<O>(&self, f: impl FnOnce(&Rc<Doc>) -> O) -> O {
        self.inner.with_untracked(|doc| {
            let doc = doc.clone();
            f(&doc)
        })
    }

    pub fn track(&self) {
        self.inner.track();
    }
}

/// Checks if completion should be triggered if the received command
/// is one that inserts whitespace or deletes whitespace
fn show_completion(
    cmd: &EditCommand,
    doc: &Rope,
    deltas: &[(Rope, RopeDelta, InvalLines)]
) -> bool {
    let show_completion = match cmd {
        EditCommand::DeleteBackward
        | EditCommand::DeleteForward
        | EditCommand::DeleteWordBackward
        | EditCommand::DeleteWordForward
        | EditCommand::DeleteForwardAndInsert => {
            let start = match deltas.first().and_then(|delta| delta.1.els.first()) {
                Some(lapce_xi_rope::DeltaElement::Copy(_, start)) => *start,
                _ => 0
            };

            let end = match deltas.first().and_then(|delta| delta.1.els.get(1)) {
                Some(lapce_xi_rope::DeltaElement::Copy(end, _)) => *end,
                _ => 0
            };

            if start > 0 && end > start {
                !doc.slice_to_cow(start..end)
                    .chars()
                    .all(|c| c.is_whitespace() || c.is_ascii_whitespace())
            } else {
                true
            }
        },
        EditCommand::NormalizeLineEndings => true,
        _ => false
    };

    show_completion
}

fn show_inline_completion(cmd: &EditCommand) -> bool {
    matches!(
        cmd,
        EditCommand::DeleteBackward
            | EditCommand::DeleteForward
            | EditCommand::DeleteWordBackward
            | EditCommand::DeleteWordForward
            | EditCommand::DeleteForwardAndInsert
            | EditCommand::IndentLine
            | EditCommand::InsertMode
    )
}

// TODO(minor): Should we just put this on view, since it only requires those
// values? pub(crate) fn compute_screen_lines(
//     // base: RwSignal<ScreenLinesBase>,
//     // view_kind: ReadSignal<EditorViewKind>,
//     doc_lines: DocLines,
//     // _config_id: ConfigId,
// ) -> ScreenLines {
//     // TODO: this should probably be a get since we need to depend on
// line-height     // let doc_lines = doc.doc_lines.get_untracked();
//     let config = doc_lines.config.get_untracked();
//     let line_height = config.editor.line_height();
//     let view_kind = doc_lines.kind.get_untracked();
//     let base = doc_lines.viewport.get_untracked();
//
//     let (y0, y1) = (base.y0, base.y1);
//     // Get the start and end (visual) lines that are visible in the viewport
//     let min_val = (y0 / line_height as f64).floor() as usize;
//     let min_vline = VLine(min_val);
//     let max_val = (y1 / line_height as f64).floor() as usize;
//
//     // let cache_rev = doc.cache_rev.get();
//     // lines.check_cache_rev(cache_rev);
//     // TODO(minor): we don't really need to depend on various subdetails that
// aren't affecting how     // the screen lines are set up, like the title of a
// scratch document.     // doc.content.track();
//     // doc.loaded.track();
//
//     match view_kind {
//         EditorViewKind::Normal => {
//             let mut rvlines = Vec::new();
//             let mut info = HashMap::new();
//
//             let vline_infos = doc_lines.vline_infos(min_val, max_val);
//
//             for vline_info in vline_infos {
//                 rvlines.push(vline_info.rvline);
//                 let y_idx = min_vline.get() + rvlines.len();
//                 let vline_y = y_idx * line_height;
//                 let line_y = vline_y - vline_info.rvline.line_index *
// line_height;
//
//                 // Add the information to make it cheap to get in the future.
//                 // This y positions are shifted by the baseline y0
//                 info.insert(
//                     vline_info.rvline,
//                     LineInfo {
//                         y: line_y as f64 - y0,
//                         vline_y: vline_y as f64 - y0,
//                         vline_info,
//                     },
//                 );
//             }
//
//             ScreenLines {
//                 lines: rvlines,
//                 info: Rc::new(info),
//                 diff_sections: None,
//                 // base,
//             }
//         }
//         EditorViewKind::Diff(_diff_info) => {
//             // TODO: let lines in diff view be wrapped, possibly screen_lines
// should be impl'd             // on DiffEditorData
//             todo!()
//             // let mut y_idx = 0;
//             // let mut rvlines = Vec::new();
//             // let mut info = HashMap::new();
//             // let mut diff_sections = Vec::new();
//             // let mut last_change: Option<&DiffLines> = None;
//             // let mut changes = diff_info.changes.iter().peekable();
//             // let is_right = diff_info.is_right;
//             //
//             // let line_y = |info: VLineInfo<()>, vline_y: usize| -> usize {
//             //     vline_y.saturating_sub(info.rvline.line_index *
// line_height)             // };
//             //
//             // while let Some(change) = changes.next() {
//             //     match (is_right, change) {
//             //         (true, DiffLines::Left(range)) => {
//             //             if let Some(DiffLines::Right(_)) = changes.peek()
// {             //             } else {
//             //                 let len = range.len();
//             //                 diff_sections.push(DiffSection {
//             //                     y_idx,
//             //                     height: len,
//             //                     kind: DiffSectionKind::NoCode,
//             //                 });
//             //                 y_idx += len;
//             //             }
//             //         }
//             //         (false, DiffLines::Right(range)) => {
//             //             let len = if let Some(DiffLines::Left(r)) =
// last_change {             //                 range.len() -
// r.len().min(range.len())             //             } else {
//             //                 range.len()
//             //             };
//             //             if len > 0 {
//             //                 diff_sections.push(DiffSection {
//             //                     y_idx,
//             //                     height: len,
//             //                     kind: DiffSectionKind::NoCode,
//             //                 });
//             //                 y_idx += len;
//             //             }
//             //         }
//             //         (true, DiffLines::Right(range))
//             //         | (false, DiffLines::Left(range)) => {
//             //             // TODO: count vline count in the range instead
//             //             let height = range.len();
//             //
//             //             diff_sections.push(DiffSection {
//             //                 y_idx,
//             //                 height,
//             //                 kind: if is_right {
//             //                     DiffSectionKind::Added
//             //                 } else {
//             //                     DiffSectionKind::Removed
//             //                 },
//             //             });
//             //
//             //             let initial_y_idx = y_idx;
//             //             // Mopve forward by the count given.
//             //             y_idx += height;
//             //
//             //             if y_idx < min_vline.get() {
//             //                 if is_right {
//             //                     if let Some(DiffLines::Left(r)) =
// last_change {             //                         // TODO: count vline
// count in the other editor since this is skipping an amount dependent on those
// vlines             //                         let len = r.len() -
// r.len().min(range.len());             //                         if len > 0 {
//             //                             diff_sections.push(DiffSection {
//             //                                 y_idx,
//             //                                 height: len,
//             //                                 kind: DiffSectionKind::NoCode,
//             //                             });
//             //                             y_idx += len;
//             //                         }
//             //                     };
//             //                 }
//             //                 last_change = Some(change);
//             //                 continue;
//             //             }
//             //
//             //             let start_rvline =
//             //                 lines.rvline_of_line(text_prov, range.start);
//             //
//             //             // TODO: this wouldn't need to produce vlines if
// screen lines didn't             //             // require them.
//             //             let iter = lines
//             //                 .iter_rvlines_init(
//             //                     text_prov,
//             //                     cache_rev,
//             //                     config_id,
//             //                     start_rvline,
//             //                     false,
//             //                 )
//             //                 .take_while(|vline_info| {
//             //                     vline_info.rvline.line < range.end
//             //                 })
//             //                 .enumerate();
//             //             for (i, rvline_info) in iter {
//             //                 let rvline = rvline_info.rvline;
//             //                 if initial_y_idx + i < min_vline.0 {
//             //                     continue;
//             //                 }
//             //
//             //                 rvlines.push(rvline);
//             //                 let vline_y = (initial_y_idx + i) *
// line_height;             //                 info.insert(
//             //                     rvline,
//             //                     LineInfo {
//             //                         y: line_y(rvline_info, vline_y) as f64
// - y0,             //                         vline_y: vline_y as f64 - y0, //
//   vline_info: rvline_info, //                     }, //                 ); //
//   //                 if initial_y_idx + i > max_vline.0 { // break; // } // }
//   // //             if is_right { //                 if let
//   Some(DiffLines::Left(r)) = last_change
// {             //                     // TODO: count vline count in the other
// editor since this is skipping an amount dependent on those vlines
// //                     let len = r.len() - r.len().min(range.len());
//             //                     if len > 0 {
//             //                         diff_sections.push(DiffSection {
//             //                             y_idx,
//             //                             height: len,
//             //                             kind: DiffSectionKind::NoCode,
//             //                         });
//             //                         y_idx += len;
//             //                     }
//             //                 };
//             //             }
//             //         }
//             //         (_, DiffLines::Both(bothinfo)) => {
//             //             let start = if is_right {
//             //                 bothinfo.right.start
//             //             } else {
//             //                 bothinfo.left.start
//             //             };
//             //             let len = bothinfo.right.len();
//             //             let diff_height = len
//             //                 - bothinfo
//             //                     .skip
//             //                     .as_ref()
//             //                     .map(|skip| skip.len().saturating_sub(1))
//             //                     .unwrap_or(0);
//             //             if y_idx + diff_height < min_vline.get() {
//             //                 y_idx += diff_height;
//             //                 last_change = Some(change);
//             //                 continue;
//             //             }
//             //
//             //             let start_rvline = lines.rvline_of_line(text_prov,
// start);             //
//             //             let mut iter = lines
//             //                 .iter_rvlines_init(
//             //                     text_prov,
//             //                     cache_rev,
//             //                     config_id,
//             //                     start_rvline,
//             //                     false,
//             //                 )
//             //                 .take_while(|info| info.rvline.line < start +
// len);             //             while let Some(rvline_info) = iter.next() {
//             //                 let line = rvline_info.rvline.line;
//             //
//             //                 // Skip over the lines
//             //                 if let Some(skip) = bothinfo.skip.as_ref() {
//             //                     if Some(skip.start) ==
// line.checked_sub(start) {             //                         y_idx += 1;
//             //                         // Skip by `skip` count
//             //                         for _ in
// 0..skip.len().saturating_sub(1) {             //
// iter.next();             //                         }
//             //                         continue;
//             //                     }
//             //                 }
//             //
//             //                 // Add the vline if it is within view
//             //                 if y_idx >= min_vline.get() {
//             //                     rvlines.push(rvline_info.rvline);
//             //                     let vline_y = y_idx * line_height;
//             //                     info.insert(
//             //                         rvline_info.rvline,
//             //                         LineInfo {
//             //                             y: line_y(rvline_info, vline_y) as
// f64 - y0,             //                             vline_y: vline_y as f64
// - y0,             //                             vline_info: rvline_info, //
//   }, //                     ); //                 } // // y_idx += 1; // //
//   if y_idx - 1 > max_vline.get() { // break; //                 } // } // }
//   //     } // last_change = Some(change); // } // ScreenLines { // lines:
//   Rc::new(rvlines), //     info: Rc::new(info), // diff_sections:
//   Some(Rc::new(diff_sections)), //     base, // } } }
// }

fn parse_hover_resp(
    hover: lsp_types::Hover,
    directory: &Directory,
    font_family: &str,
    editor_fg: Color,
    style_colors: &HashMap<String, Color>,
    font_size: f32,
    markdown_blockquote: Color,
    editor_link: Color
) -> Vec<MarkdownContent> {
    match hover.contents {
        HoverContents::Scalar(text) => match text {
            MarkedString::String(text) => parse_markdown(
                &text,
                1.8,
                directory,
                font_family,
                editor_fg,
                style_colors,
                font_size,
                markdown_blockquote,
                editor_link
            ),
            MarkedString::LanguageString(code) => parse_markdown(
                &format!("```{}\n{}\n```", code.language, code.value),
                1.8,
                directory,
                font_family,
                editor_fg,
                style_colors,
                font_size,
                markdown_blockquote,
                editor_link
            )
        },
        HoverContents::Array(array) => array
            .into_iter()
            .map(|t| {
                from_marked_string(
                    t,
                    directory,
                    font_family,
                    editor_fg,
                    style_colors,
                    font_size,
                    markdown_blockquote,
                    editor_link
                )
            })
            .rev()
            .reduce(|mut contents, more| {
                contents.push(MarkdownContent::Separator);
                contents.extend(more);
                contents
            })
            .unwrap_or_default(),
        HoverContents::Markup(content) => match content.kind {
            MarkupKind::PlainText => from_plaintext(&content.value, 1.8, font_size),
            MarkupKind::Markdown => parse_markdown(
                &content.value,
                1.8,
                directory,
                font_family,
                editor_fg,
                style_colors,
                font_size,
                markdown_blockquote,
                editor_link
            )
        }
    }
}
