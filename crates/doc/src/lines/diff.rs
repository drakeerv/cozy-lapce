use std::ops::Range;

use serde::{Deserialize, Serialize};

use super::PeekDiff;
use crate::lines::buffer::diff::DiffLines;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiffInfo {
    pub is_right: bool,
    pub changes:  Vec<DiffLines>,
}
#[derive(Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub enum DiffResult {
    /// 对方新增/已方删除
    Empty { lines: Range<usize> },
    /// 双方修改
    Changed { lines: Range<usize> },
}

pub fn is_empty(rs: &&DiffResult) -> bool {
    matches!(rs, DiffResult::Empty { .. })
}
pub fn is_changed(rs: &&DiffResult) -> bool {
    matches!(rs, DiffResult::Changed { .. })
}

impl DiffResult {
    pub fn line(&self) -> &Range<usize> {
        match self {
            DiffResult::Empty { lines: line } => line,
            DiffResult::Changed { lines: line, .. } => line,
        }
    }

    pub fn consume_line(&self, line: &usize) -> Option<Range<usize>> {
        match self {
            DiffResult::Empty { lines } => {
                if lines.contains(line) {
                    Some(lines.clone())
                } else {
                    None
                }
            },
            DiffResult::Changed { .. } => None,
        }
    }

    pub fn is_diff(&self, line: &usize) -> bool {
        match self {
            DiffResult::Empty { .. } => false,
            DiffResult::Changed { lines, .. } => lines.contains(line),
        }
    }
}

impl DiffInfo {
    pub fn changes(&self) -> Vec<DiffResult> {
        if self.is_right {
            self.right_changes()
        } else {
            self.left_changes()
        }
    }

    pub fn left_changes(&self) -> Vec<DiffResult> {
        // log::info!("{}", serde_json::to_string(&self.changes).unwrap());
        let mut changes = self.changes.iter().peekable();
        let mut next_left_change_line: Option<Range<usize>> = None;
        let mut diff_tys = vec![];
        while let Some(change) = changes.next() {
            match change {
                DiffLines::Left(diff) => {
                    next_left_change_line = Some(diff.clone());
                    diff_tys.push(DiffResult::Changed {
                        lines: diff.clone(),
                    });
                    if let Some(DiffLines::Right(right_diff)) = changes.peek() {
                        // edit
                        if diff.len() < right_diff.len() {
                            diff_tys.push(DiffResult::Empty {
                                lines: diff.end
                                    ..diff.end + right_diff.len() - diff.len(),
                            });
                        }
                        changes.next();
                    }
                },
                DiffLines::Both(diff) => {
                    next_left_change_line = Some(diff.left.clone());
                },
                DiffLines::Right(diff) => {
                    diff_tys.push(match &next_left_change_line {
                        None => DiffResult::Empty {
                            lines: 0..diff.len(),
                        },
                        Some(lines) => DiffResult::Empty {
                            lines: lines.end..lines.end + diff.len(),
                        },
                    });
                },
            }
        }
        // log::info!("{}", serde_json::to_string(&diff_tys).unwrap());

        diff_tys
    }

    pub fn right_changes(&self) -> Vec<DiffResult> {
        // log::info!("{}", serde_json::to_string(&self.changes).unwrap());
        let mut changes = self.changes.iter().peekable();
        let mut next_right_change_line: Option<Range<usize>> = None;
        let mut diff_tys = vec![];

        while let Some(change) = changes.next() {
            match change {
                DiffLines::Left(left_diff) => {
                    if let Some(DiffLines::Right(diff)) = changes.peek() {
                        // edit
                        changes.next();
                        diff_tys.push(DiffResult::Changed {
                            lines: diff.clone(),
                        });
                        if diff.len() < left_diff.len() {
                            diff_tys.push(DiffResult::Empty {
                                lines: diff.end
                                    ..diff.end + left_diff.len() - diff.len(),
                            });
                        }
                        next_right_change_line = Some(diff.clone());
                    } else {
                        diff_tys.push(match &next_right_change_line {
                            None => DiffResult::Empty {
                                lines: 0..left_diff.len(),
                            },
                            Some(lines) => DiffResult::Empty {
                                lines: lines.end..lines.end + left_diff.len(),
                            },
                        })
                    }
                },
                DiffLines::Both(diff) => {
                    next_right_change_line = Some(diff.right.clone());
                },
                DiffLines::Right(diff) => {
                    diff_tys.push(match &next_right_change_line {
                        None => DiffResult::Changed {
                            lines: 0..diff.len(),
                        },
                        Some(lines) => DiffResult::Changed {
                            lines: lines.end..lines.end + diff.len(),
                        },
                    });
                },
            }
        }
        // log::info!("{}", serde_json::to_string(&diff_tys).unwrap());
        diff_tys
    }
}

pub fn is_diff(changes: &mut PeekDiff, line: usize) -> bool {
    loop {
        if let Some(diff) = changes.peek() {
            if diff.line().end <= line {
                changes.next();
                continue;
            } else {
                return diff.is_diff(&line);
            }
        } else {
            return false;
        }
    }
}

pub fn advance(changes: &mut PeekDiff, line_index: usize) {
    if let Some(diff) = changes.peek() {
        if diff.line().end <= line_index {
            changes.next();
        }
    }
}

pub fn consume_line(
    changes: &mut PeekDiff,
    line_index: usize,
) -> Option<Range<usize>> {
    if let Some(diff) = changes.peek() {
        diff.consume_line(&line_index)
    } else {
        None
    }
}

// pub fn consume_lines_until_enough(
//     changes: &mut Peekable<Filter<Iter<DiffResult>, fn(&&DiffResult) ->
// bool>>,     end_index: usize
// ) -> usize {
//     let mut index = 0;
//     let mut line = 0;
//     loop {
//         if index >= end_index {
//             break;
//         }
//         if consume_line(changes, line).is_some() {
//             index += 1;
//             continue;
//         }
//         line += 1;
//         index += 1;
//     }
//     line
// }
