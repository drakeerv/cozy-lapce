use std::{rc::Rc, sync::atomic};

use doc::{
    EditorViewKind,
    lines::{
        buffer::{
            diff::{DiffExpand, DiffLines, expand_diff_lines, rope_diff},
            rope_text::RopeText,
        },
        diff::DiffInfo,
    },
};
use floem::{
    View,
    event::EventListener,
    ext_event::create_ext_action,
    reactive::{RwSignal, Scope, SignalGet, SignalUpdate, SignalWith},
    style::CursorStyle,
    views::{Decorators, clip, dyn_stack, empty, label, stack},
};
use lapce_core::{
    editor_tab::DiffEditorInfo,
    icon::LapceIcons,
    id::{DiffEditorId, EditorId, EditorTabManageId},
};

use super::EditorData;
use crate::{
    config::color::LapceColor, doc::Doc, main_split::Editors, svg, wave::wave_box,
    window_workspace::CommonData,
};

// #[derive(Clone)]
// pub struct DiffInfo {
//     pub is_right: bool,
//     pub changes: Vec<DiffLines>,
// }

#[derive(Clone)]
pub struct DiffEditorData {
    pub id: DiffEditorId,
    pub editor_tab_id: RwSignal<EditorTabManageId>,
    pub scope: Scope,
    pub left: EditorData,
    pub right: EditorData,
    // pub confirmed:     RwSignal<bool>,
    pub focus_right: RwSignal<bool>,
}

impl DiffEditorData {
    pub fn new(
        cx: Scope,
        id: DiffEditorId,
        editor_tab_id: EditorTabManageId,
        left_doc: Rc<Doc>,
        right_doc: Rc<Doc>,
        editors: Editors,
        common: Rc<CommonData>,
    ) -> Self {
        let cx = cx.create_child();

        // TODO: ensure that left/right are cleaned up
        let [left, right] = [
            editors.make_from_doc(
                cx,
                left_doc,
                None,
                Some((editor_tab_id, id)),
                common.clone(),
                EditorViewKind::Diff(DiffInfo {
                    is_right: false,
                    changes: vec![],
                }),
            ),
            editors.make_from_doc(
                cx,
                right_doc,
                None,
                Some((editor_tab_id, id)),
                common.clone(),
                EditorViewKind::Diff(DiffInfo {
                    is_right: true,
                    changes: vec![],
                }),
            ),
        ];

        let data = Self {
            id,
            editor_tab_id: cx.create_rw_signal(editor_tab_id),
            scope: cx,
            left,
            right,
            focus_right: cx.create_rw_signal(true),
        };

        data.listen_diff_changes();

        data
    }

    pub fn diff_editor_info(&self) -> DiffEditorInfo {
        DiffEditorInfo {
            left_content: self.left.doc().content.get_untracked(),
            right_content: self.right.doc().content.get_untracked(),
        }
    }

    pub fn copy(
        &self,
        cx: Scope,
        editor_tab_id: EditorTabManageId,
        diff_editor_id: EditorId,
        editors: Editors,
    ) -> Self {
        let cx = cx.create_child();

        let [left, right] = [&self.left, &self.right].map(|editor_data| {
            editors
                .make_copy(
                    editor_data.id(),
                    cx,
                    None,
                    Some((editor_tab_id, diff_editor_id)),
                )
                .unwrap()
        });

        let diff_editor = DiffEditorData {
            scope: cx,
            id: diff_editor_id,
            editor_tab_id: cx.create_rw_signal(editor_tab_id),
            focus_right: cx.create_rw_signal(true),
            left,
            right,
        };

        diff_editor.listen_diff_changes();
        diff_editor
    }

    fn listen_diff_changes(&self) {
        let cx = self.scope;

        let left = self.left.clone();
        let left_doc_rev = {
            let left = left.clone();
            cx.create_memo(move |_| {
                let doc = left.doc_signal().get();
                let buffer = doc.lines.with_untracked(|x| x.signal_buffer());
                (doc.content.get(), buffer.with(|b| b.rev()))
            })
        };

        let right = self.right.clone();
        let right_doc_rev = {
            let right = right.clone();
            cx.create_memo(move |_| {
                let doc = right.doc_signal().get();
                let buffer = doc.lines.with_untracked(|x| x.signal_buffer());
                (doc.content.get(), buffer.with(|b| b.rev()))
            })
        };

        cx.create_effect(move |_| {
            let (_, left_rev) = left_doc_rev.get();
            let (left_editor_view, left_doc) = (left.kind_rw(), left.doc());
            let (left_atomic_rev, left_rope) =
                left_doc.lines.with_untracked(|buffer| {
                    (buffer.buffer().atomic_rev(), buffer.buffer().text().clone())
                });

            let (_, right_rev) = right_doc_rev.get();
            let (right_editor_view, right_doc) = (right.kind_rw(), right.doc());
            let (right_atomic_rev, right_rope) =
                right_doc.lines.with_untracked(|buffer| {
                    (buffer.buffer().atomic_rev(), buffer.buffer().text().clone())
                });

            let send = {
                let right_atomic_rev = right_atomic_rev.clone();
                create_ext_action(cx, move |changes: Option<Vec<DiffLines>>| {
                    let changes = if let Some(changes) = changes {
                        changes
                    } else {
                        return;
                    };

                    if left_atomic_rev.load(atomic::Ordering::Acquire) != left_rev {
                        return;
                    }

                    if right_atomic_rev.load(atomic::Ordering::Acquire) != right_rev
                    {
                        return;
                    }

                    left_editor_view.set(EditorViewKind::Diff(DiffInfo {
                        is_right: false,
                        changes: changes.clone(),
                    }));
                    right_editor_view.set(EditorViewKind::Diff(DiffInfo {
                        is_right: true,
                        changes,
                    }));
                })
            };

            rayon::spawn(move || {
                let changes = rope_diff(
                    left_rope,
                    right_rope,
                    right_rev,
                    right_atomic_rev.clone(),
                    Some(3),
                );
                send(changes);
            });
        });
    }
}

struct DiffShowMoreSection {
    left_actual_line: usize,
    right_actual_line: usize,
    visual_line: usize,
    lines: usize,
}

pub fn diff_show_more_section_view(
    left_editor: &EditorData,
    right_editor: &EditorData,
) -> impl View {
    let left_editor_view = left_editor.kind_rw();
    let right_editor_view = right_editor.kind_rw();
    let viewport = right_editor.editor.viewport;
    let config = right_editor.common.config;
    let line_height = right_editor.common.ui_line_height;

    let each_fn = move || {
        let editor_view = right_editor_view.get();
        if let EditorViewKind::Diff(diff_info) = editor_view {
            let viewport = viewport.get();
            let line_height =
                config.with_untracked(|config| config.editor.line_height() as f64);

            let min_line = (viewport.y0 / line_height).floor() as usize;
            let max_line = (viewport.y1 / line_height).ceil() as usize;

            let mut visual_line = 0;
            let mut last_change: Option<&DiffLines> = None;
            let mut changes = diff_info.changes.iter().peekable();
            let mut sections = Vec::new();
            while let Some(change) = changes.next() {
                match change {
                    DiffLines::Left(range) => {
                        if let Some(DiffLines::Right(_)) = changes.peek() {
                        } else {
                            let len = range.len();
                            visual_line += len;
                        }
                    },
                    DiffLines::Right(range) => {
                        let len = range.len();
                        visual_line += len;

                        if let Some(DiffLines::Left(r)) = last_change {
                            let len = r.len() - r.len().min(range.len());
                            if len > 0 {
                                visual_line += len;
                            }
                        };
                    },
                    DiffLines::Both(info) => {
                        if let Some(skip) = info.skip.as_ref() {
                            visual_line += skip.start;
                            if visual_line + 1 >= min_line {
                                sections.push(DiffShowMoreSection {
                                    left_actual_line: info.left.start,
                                    right_actual_line: info.right.start,
                                    visual_line,
                                    lines: skip.len(),
                                });
                            }
                            visual_line += 1;
                            visual_line += info.right.len() - skip.end;
                        } else {
                            visual_line += info.right.len();
                        }
                    },
                }
                if visual_line > max_line {
                    break;
                }
                last_change = Some(change);
            }
            sections
        } else {
            Vec::new()
        }
    };

    let key_fn =
        move |section: &DiffShowMoreSection| (section.visual_line, section.lines);

    let view_fn = move |section: DiffShowMoreSection| {
        stack((
            wave_box().style(move |s| {
                s.absolute()
                    .size_pct(100.0, 100.0)
                    .color(config.with_color(LapceColor::PANEL_BACKGROUND))
            }),
            label(move || format!("{} Hidden Lines", section.lines)),
            label(|| "|".to_string()).style(|s| s.margin_left(10.0)),
            stack((
                svg(move || config.with_ui_svg(LapceIcons::FOLD)).style(move |s| {
                    let (caret_color, size) = config.with(|config| {
                        (
                            config.color(LapceColor::EDITOR_FOREGROUND),
                            config.ui.icon_size() as f32,
                        )
                    });
                    s.size(size, size).color(caret_color)
                }),
                label(|| "Expand All".to_string()).style(|s| s.margin_left(6.0)),
            ))
            .on_event_stop(EventListener::PointerDown, move |_| {})
            .on_click_stop(move |_event| {
                left_editor_view.update(|editor_view| {
                    if let EditorViewKind::Diff(diff_info) = editor_view {
                        expand_diff_lines(
                            &mut diff_info.changes,
                            section.left_actual_line,
                            DiffExpand::All,
                            false,
                        );
                    }
                });
                right_editor_view.update(|editor_view| {
                    if let EditorViewKind::Diff(diff_info) = editor_view {
                        expand_diff_lines(
                            &mut diff_info.changes,
                            section.right_actual_line,
                            DiffExpand::All,
                            true,
                        );
                    }
                });
            })
            .style(|s| {
                s.margin_left(10.0)
                    .height_pct(100.0)
                    .items_center()
                    .hover(|s| s.cursor(CursorStyle::Pointer))
            }),
            label(|| "|".to_string()).style(|s| s.margin_left(10.0)),
            stack((
                svg(move || config.with_ui_svg(LapceIcons::FOLD_UP)).style(
                    move |s| {
                        let (caret_color, size) = config.with(|config| {
                            (
                                config.color(LapceColor::EDITOR_FOREGROUND),
                                config.ui.icon_size() as f32,
                            )
                        });
                        s.size(size, size).color(caret_color)
                    },
                ),
                label(|| "Expand Up".to_string()).style(|s| s.margin_left(6.0)),
            ))
            .on_event_stop(EventListener::PointerDown, move |_| {})
            .on_click_stop(move |_event| {
                left_editor_view.update(|editor_view| {
                    if let EditorViewKind::Diff(diff_info) = editor_view {
                        expand_diff_lines(
                            &mut diff_info.changes,
                            section.left_actual_line,
                            DiffExpand::Up(10),
                            false,
                        );
                    }
                });
                right_editor_view.update(|editor_view| {
                    if let EditorViewKind::Diff(diff_info) = editor_view {
                        expand_diff_lines(
                            &mut diff_info.changes,
                            section.right_actual_line,
                            DiffExpand::Up(10),
                            true,
                        );
                    }
                });
            })
            .style(move |s| {
                s.margin_left(10.0)
                    .height_pct(100.0)
                    .items_center()
                    .hover(|s| s.cursor(CursorStyle::Pointer))
            }),
            label(|| "|".to_string()).style(|s| s.margin_left(10.0)),
            stack((
                svg(move || config.with_ui_svg(LapceIcons::FOLD_DOWN)).style(
                    move |s| {
                        let (caret_color, size) = config.with(|config| {
                            (
                                config.color(LapceColor::EDITOR_FOREGROUND),
                                config.ui.icon_size() as f32,
                            )
                        });
                        s.size(size, size).color(caret_color)
                    },
                ),
                label(|| "Expand Down".to_string()).style(|s| s.margin_left(6.0)),
            ))
            .on_event_stop(EventListener::PointerDown, move |_| {})
            .on_click_stop(move |_event| {
                left_editor_view.update(|editor_view| {
                    if let EditorViewKind::Diff(diff_info) = editor_view {
                        expand_diff_lines(
                            &mut diff_info.changes,
                            section.left_actual_line,
                            DiffExpand::Down(10),
                            false,
                        );
                    }
                });
                right_editor_view.update(|editor_view| {
                    if let EditorViewKind::Diff(diff_info) = editor_view {
                        expand_diff_lines(
                            &mut diff_info.changes,
                            section.right_actual_line,
                            DiffExpand::Down(10),
                            true,
                        );
                    }
                });
            })
            .style(move |s| {
                s.margin_left(10.0)
                    .height_pct(100.0)
                    .items_center()
                    .hover(|s| s.cursor(CursorStyle::Pointer))
            }),
        ))
        .style(move |s| {
            let line_height = line_height.get();
            s.absolute()
                .width_pct(100.0)
                .height(line_height)
                .justify_center()
                .items_center()
                .margin_top(
                    (section.visual_line as f32) * line_height as f32
                        - viewport.get().y0 as f32,
                )
                .hover(|s| s.cursor(CursorStyle::Default))
        })
    };

    stack((
        empty().style(move |s| s.height(line_height.get() as f32 + 1.0)),
        clip(
            dyn_stack(each_fn, key_fn, view_fn)
                .style(|s| s.flex_col().size_pct(100.0, 100.0)),
        )
        .style(|s| s.size_pct(100.0, 100.0)),
    ))
    .style(|s| s.absolute().flex_col().size_pct(100.0, 100.0))
    .debug_name("Diff Show More Section")
}
