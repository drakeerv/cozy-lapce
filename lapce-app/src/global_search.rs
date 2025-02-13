use std::{ops::Range, path::PathBuf, rc::Rc};

use doc::lines::{editor_command::CommandExecuted, mode::Mode};
use floem::{
    ext_event::create_ext_action,
    keyboard::Modifiers,
    reactive::{Memo, RwSignal, Scope, SignalGet, SignalUpdate, SignalWith},
    views::VirtualVector
};
use indexmap::IndexMap;
use lapce_rpc::proxy::{ProxyResponse, SearchMatch};

use crate::{
    keypress::{KeyPressFocus, condition::Condition},
    main_split::MainSplitData,
    window_workspace::CommonData
};

#[derive(Clone)]
pub struct SearchMatchData {
    pub expanded:    RwSignal<bool>,
    pub matches:     RwSignal<im::Vector<SearchMatch>>,
    pub line_height: Memo<f64>
}

impl SearchMatchData {
    pub fn height(&self) -> f64 {
        let line_height = self.line_height.get();
        let count = if self.expanded.get() {
            self.matches.with(|m| m.len()) + 1
        } else {
            1
        };
        line_height * count as f64
    }
}

#[derive(Clone, Debug)]
pub struct GlobalSearchData {
    pub search_result: RwSignal<IndexMap<PathBuf, SearchMatchData>>,
    pub search_str:    RwSignal<String>,
    pub main_split:    MainSplitData,
    pub common:        Rc<CommonData>
}

impl KeyPressFocus for GlobalSearchData {
    fn get_mode(&self) -> Mode {
        Mode::Insert
    }

    fn check_condition(&self, condition: Condition) -> bool {
        matches!(condition, Condition::PanelFocus)
    }

    fn run_command(
        &self,
        _command: &crate::command::LapceCommand,
        _count: Option<usize>,
        _mods: Modifiers
    ) -> CommandExecuted {
        CommandExecuted::No
    }

    fn receive_char(&self, _c: &str) {}
}

impl VirtualVector<(PathBuf, SearchMatchData)> for GlobalSearchData {
    fn total_len(&self) -> usize {
        self.search_result.with(|result| {
            result
                .iter()
                .map(|(_, data)| {
                    if data.expanded.get() {
                        data.matches.with(|m| m.len()) + 1
                    } else {
                        1
                    }
                })
                .sum()
        })
    }

    fn slice(
        &mut self,
        _range: Range<usize>
    ) -> impl Iterator<Item = (PathBuf, SearchMatchData)> {
        self.search_result.get().into_iter()
    }
}

impl GlobalSearchData {
    pub fn new(cx: Scope, main_split: MainSplitData) -> Self {
        let common = main_split.common.clone();
        let search_result = cx.create_rw_signal(IndexMap::new());
        let search_str = cx.create_rw_signal(String::new());

        let global_search = Self {
            search_result,
            main_split,
            common,
            search_str
        };

        {
            let buffer = global_search.search_str;
            let global_search = global_search.clone();
            cx.create_effect(move |_| {
                let pattern = buffer.get();
                if pattern.is_empty() {
                    global_search.search_result.update(|r| r.clear());
                    return;
                }
                let case_sensitive = global_search.common.find.case_sensitive(true);
                let whole_word = global_search.common.find.whole_words.get();
                let is_regex = global_search.common.find.is_regex.get();
                let send = {
                    let global_search = global_search.clone();
                    create_ext_action(cx, move |result| {
                        if let Ok(ProxyResponse::GlobalSearchResponse { matches }) =
                            result
                        {
                            global_search.update_matches(matches);
                        }
                    })
                };
                global_search.common.proxy.global_search(
                    pattern,
                    case_sensitive,
                    whole_word,
                    is_regex,
                    move |(_, result)| {
                        send(result);
                    }
                );
            });
        }

        // why? this will display find view
        // {
        //     let buffer = global_search.editor.doc().buffer;
        //     let main_split = global_search.main_split.clone();
        //     cx.create_effect(move |_| {
        //         let content = buffer.with(|buffer| buffer.to_string());
        //         main_split.set_find_pattern(Some(content));
        //     });
        // }

        global_search
    }

    fn update_matches(&self, matches: IndexMap<PathBuf, Vec<SearchMatch>>) {
        let current = self.search_result.get_untracked();

        self.search_result.set(
            matches
                .into_iter()
                .map(|(path, matches)| {
                    let match_data =
                        current.get(&path).cloned().unwrap_or_else(|| {
                            SearchMatchData {
                                expanded:    self
                                    .common
                                    .scope
                                    .create_rw_signal(true),
                                matches:     self
                                    .common
                                    .scope
                                    .create_rw_signal(im::Vector::new()),
                                line_height: self.common.ui_line_height
                            }
                        });

                    match_data.matches.set(matches.into());

                    (path, match_data)
                })
                .collect()
        );
    }

    pub fn set_pattern(&self, pattern: String) {
        self.search_str.set(pattern);
    }
}
