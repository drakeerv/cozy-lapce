use std::{cmp::Ordering, iter::Peekable, ops::RangeInclusive, slice::Iter};

use anyhow::Result;
use floem::peniko::Color;
use im::HashMap;
use lapce_xi_rope::{Interval, Rope};
use log::error;
use lsp_types::Position;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;

use super::phantom_text::{PhantomText, PhantomTextKind};
use crate::lines::{
    buffer::{Buffer, rope_text::RopeText},
    screen_lines::ScreenLines,
};

pub struct FoldingRangesLine<'a> {
    folding: Peekable<Iter<'a, FoldedRange>>,
}

pub struct MergeFoldingRangesLine<'a> {
    folding: Peekable<Iter<'a, FoldedRange>>,
}

impl<'a> MergeFoldingRangesLine<'a> {
    pub fn new(folding: &'a Vec<FoldedRange>) -> Self {
        let folding = folding.iter().peekable();
        Self { folding }
    }

    /// 计算line在实际展示时，位于第几行
    pub fn get_line_num(
        &mut self,
        origin_folded_index: usize,
        last_line: usize,
    ) -> Option<usize> {
        let mut index = 0;
        let mut line_num = 0;
        while line_num <= last_line {
            if index == origin_folded_index {
                return Some(line_num);
            }
            if let Some(folded) = self.get_folded_range_by_line(line_num as u32) {
                line_num = *folded.end() + 1;
            } else {
                line_num += 1;
            }
            index += 1;
        }
        None
    }

    pub fn get_folded_range_by_line(
        &mut self,
        line: u32,
    ) -> Option<RangeInclusive<usize>> {
        loop {
            if let Some(folded) = self.folding.peek() {
                if folded.end.line < line {
                    self.folding.next();
                    continue;
                } else if folded.start.line <= line && line <= folded.end.line {
                    let start_line = folded.start.line;
                    let mut end_line = folded.end.line;
                    self.folding.next();
                    while let Some(next_folded) = self.folding.peek() {
                        if next_folded.start.line == end_line {
                            end_line = next_folded.end.line;
                            self.folding.next();
                            continue;
                        } else {
                            break;
                        }
                    }
                    return Some(start_line as usize..=end_line as usize);
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }
    }

    /// 计算line在实际展示时，位于第几行
    pub fn get_origin_folded_line_index(&mut self, line: usize) -> usize {
        let mut index = 0;
        let mut line_num = 0;
        while line_num <= line {
            if line_num >= line {
                break;
            }
            if let Some(folded) = self.get_folded_range_by_line(line_num as u32) {
                line_num = *folded.end() + 1;
            } else {
                line_num += 1;
            }
            index += 1;
        }
        index
    }
}
impl<'a> FoldingRangesLine<'a> {
    pub fn new(folding: &'a Vec<FoldedRange>) -> Self {
        let folding = folding.iter().peekable();
        Self { folding }
    }

    pub fn get_folded_range_by_line(
        &mut self,
        line: u32,
    ) -> Option<RangeInclusive<usize>> {
        loop {
            if let Some(folded) = self.folding.peek() {
                if folded.end.line < line {
                    self.folding.next();
                    continue;
                } else if folded.start.line <= line && line <= folded.end.line {
                    let start_line = folded.start.line;
                    let mut end_line = folded.end.line;
                    self.folding.next();
                    while let Some(next_folded) = self.folding.peek() {
                        if next_folded.start.line == end_line {
                            end_line = next_folded.end.line;
                            self.folding.next();
                            continue;
                        } else {
                            break;
                        }
                    }
                    return Some(start_line as usize..=end_line as usize);
                } else {
                    return None;
                }
            } else {
                return None;
            }
        }
    }

    pub fn contain_position(&mut self, position: Position) -> bool {
        loop {
            if let Some(folded) = self.folding.peek() {
                if folded.end.line < position.line {
                    self.folding.next();
                    continue;
                } else if folded.start <= position && position <= folded.end {
                    return true;
                } else {
                    return false;
                }
            } else {
                return false;
            }
        }
    }

    pub fn phantom_text(
        &mut self,
        line: u32,
        buffer: &Buffer,
        inlay_hint_font_size: usize,
        inlay_hint_foreground: Color,
        inlay_hint_background: Color,
    ) -> Result<SmallVec<[PhantomText; 6]>> {
        let mut textes = SmallVec::<[PhantomText; 6]>::new();
        loop {
            if let Some(folded) = self.folding.peek() {
                if folded.end.line < line {
                    self.folding.next();
                    continue;
                } else if folded.start.line == line {
                    let same_line = folded.start.line == folded.end.line;
                    let Some(start_char) =
                        buffer.char_at_offset(get_offset(buffer, folded.start)?)
                    else {
                        self.folding.next();
                        continue;
                    };
                    let Some(end_char) =
                        buffer.char_at_offset(get_offset(buffer, folded.end)? - 1)
                    else {
                        self.folding.next();
                        continue;
                    };

                    let mut text = String::new();
                    text.push(start_char);
                    text.push_str("...");
                    text.push(end_char);
                    let next_line = if same_line {
                        None
                    } else {
                        Some(folded.end.line as usize)
                    };
                    let start = folded.start.character as usize;
                    let (all_len, len) = if same_line {
                        (
                            folded.end.character as usize - start,
                            folded.end.character as usize - start,
                        )
                    } else {
                        let folded_end =
                            buffer.offset_of_line(folded.end.line as usize)?;
                        let current =
                            buffer.offset_of_line(folded.start.line as usize)?;
                        let content = buffer.line_content(line as usize)?.len();
                        (folded_end - current - start, content - start)
                    };
                    textes.push(PhantomText {
                        kind: PhantomTextKind::LineFoldedRang {
                            next_line,
                            len,
                            all_len,
                            start_position: folded.start,
                        },
                        col: start,
                        text,
                        fg: Some(inlay_hint_foreground),
                        font_size: Some(inlay_hint_font_size),
                        bg: Some(inlay_hint_background),
                        under_line: None,
                        final_col: start,
                        line: line as usize,
                        visual_merge_col: start,
                        origin_merge_col: start,
                    });
                    if !same_line {
                        break;
                    } else {
                        self.folding.next();
                    }
                } else if folded.end.line == line {
                    let text = String::new();
                    textes.push(PhantomText {
                        kind: PhantomTextKind::LineFoldedRang {
                            next_line:      None,
                            len:            folded.end.character as usize,
                            all_len:        folded.end.character as usize,
                            start_position: folded.start,
                        },
                        col: 0,
                        text,
                        fg: None,
                        font_size: None,
                        bg: None,
                        under_line: None,
                        final_col: 0,
                        line: line as usize,
                        visual_merge_col: 0,
                        origin_merge_col: 0,
                    });
                    self.folding.next();
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        Ok(textes)
    }
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub struct FoldingRanges(pub Vec<FoldingRange>);

#[derive(Default, Clone, Debug)]
pub struct FoldedRanges(pub Vec<FoldedRange>);

impl FoldingRanges {
    /// 将衔接在一起的range合并在一条中，这样便于找到合并行的起始行
    pub fn get_all_folded_folded_range(&self) -> FoldedRanges {
        let mut range = Vec::new();
        let mut limit_line = 0;
        let mut peek = self.0.iter().peekable();
        while let Some(item) = peek.next() {
            if item.start.line < limit_line && limit_line > 0 {
                continue;
            }
            if item.status.is_folded() {
                let start = item.start;
                let mut end = item.end;
                while let Some(next_item) = peek.peek() {
                    if item.end.line == next_item.start.line {
                        end = next_item.end;
                        peek.next();
                    } else {
                        break;
                    }
                }
                range.push(FoldedRange { start, end });
                limit_line = end.line;
            }
        }
        FoldedRanges(range)
    }

    pub fn get_all_folded_range(&self) -> FoldedRanges {
        // 不能合并，因为后续是一行一行拼接的。合并会导致中间行缺失
        let mut range = Vec::new();
        let mut limit_line = 0;
        for item in &self.0 {
            if item.start.line < limit_line && limit_line > 0 {
                continue;
            }
            if item.status.is_folded() {
                range.push(FoldedRange {
                    start: item.start,
                    end:   item.end,
                });
                limit_line = item.end.line;
            }
        }

        FoldedRanges(range)
    }

    // pub fn get_folded_range_by_line(&self, line: u32) -> FoldedRanges {
    //     let mut range = Vec::new();
    //     let mut limit_line = 0;
    //     for item in &self.0 {
    //         if item.start.line < limit_line && limit_line > 0 {
    //             continue;
    //         }
    //         if item.status.is_folded()
    //             && item.start.line <= line
    //             && item.end.line >= line
    //         {
    //             range.push(FoldedRange {
    //                 start:          item.start,
    //                 end:            item.end,
    //                 collapsed_text: item.collapsed_text.clone()
    //             });
    //             limit_line = item.end.line;
    //         }
    //     }
    //
    //     FoldedRanges(range)
    // }

    pub fn fold_by_offset(
        &mut self,
        offset: usize,
        rope: &Rope,
    ) -> Result<Option<usize>> {
        for item in self.0.iter_mut() {
            let start = rope.offset_of_line(item.start.line as usize)?
                + item.start.character as usize;
            let end = rope.offset_of_line(item.end.line as usize)?
                + item.end.character as usize;
            if start <= offset && offset < end {
                item.status = FoldingRangeStatus::Fold;
                return Ok(Some(start));
            } else if end < offset {
                continue;
            } else {
                break;
            }
        }
        Ok(None)
    }

    pub fn to_display_items(&self, lines: &ScreenLines) -> Vec<FoldingDisplayItem> {
        let mut folded = HashMap::new();
        let mut unfold_start: HashMap<u32, FoldingDisplayItem> = HashMap::new();
        let mut unfold_end = HashMap::new();
        let mut limit_line = 0;
        for item in &self.0 {
            if item.start.line < limit_line && limit_line > 0 {
                continue;
            }
            match item.status {
                FoldingRangeStatus::Fold => {
                    if let Some(line) = lines
                        .visual_line_info_for_origin_line(item.start.line as usize)
                    {
                        folded.insert(
                            item.start.line,
                            FoldingDisplayItem {
                                position: item.start,
                                y:        line.folded_line_y() as i32,
                                ty:       FoldingDisplayType::Folded,
                            },
                        );
                    }
                    limit_line = item.end.line;
                },
                FoldingRangeStatus::Unfold => {
                    {
                        if let Some(line) = lines.visual_line_info_for_origin_line(
                            item.start.line as usize,
                        ) {
                            unfold_start.insert(
                                item.start.line,
                                FoldingDisplayItem {
                                    position: item.start,
                                    y:        line.folded_line_y() as i32,
                                    ty:       FoldingDisplayType::UnfoldStart,
                                },
                            );
                        }
                    }
                    {
                        if let Some(line) = lines
                            .visual_line_info_for_origin_line(item.end.line as usize)
                        {
                            unfold_end.insert(
                                item.end.line,
                                FoldingDisplayItem {
                                    position: item.end,
                                    y:        line.folded_line_y() as i32,
                                    ty:       FoldingDisplayType::UnfoldEnd,
                                },
                            );
                        }
                    }
                    limit_line = 0;
                },
            };
        }
        for (key, val) in unfold_end {
            unfold_start.insert(key, val);
        }
        for (key, val) in folded {
            unfold_start.insert(key, val);
        }
        let mut items: Vec<FoldingDisplayItem> =
            unfold_start.into_iter().map(|x| x.1).collect();
        items.sort_by(|x, y| {
            let line_rs = x.position.line.cmp(&y.position.line);
            if let Ordering::Equal = line_rs {
                x.position.character.cmp(&y.position.character)
            } else {
                line_rs
            }
        });
        items
    }

    pub fn update_ranges(&mut self, mut new: Vec<FoldingRange>) {
        let folded_range = self.get_all_folded_range();
        new.iter_mut().for_each(|x| folded_range.update_status(x));
        self.0 = new;
    }

    pub fn update_folding_item(&mut self, item: FoldingDisplayItem) {
        match item.ty {
            FoldingDisplayType::UnfoldStart | FoldingDisplayType::Folded => {
                self.0.iter_mut().find_map(|range| {
                    if range.start == item.position {
                        range.status.click();
                        Some(())
                    } else {
                        None
                    }
                });
            },
            FoldingDisplayType::UnfoldEnd => {
                self.0.iter_mut().find_map(|range| {
                    if range.end == item.position {
                        range.status.click();
                        Some(())
                    } else {
                        None
                    }
                });
            },
        }
    }

    pub fn update_by_phantom(&mut self, position: Position) {
        self.0.iter_mut().find_map(|range| {
            if range.start == position {
                range.status.click();
                Some(())
            } else {
                None
            }
        });
    }
}

impl FoldedRanges {
    pub fn folded_line_count(&self) -> usize {
        self.0.iter().fold(0usize, |count, item| {
            count + item.end.line as usize - item.start.line as usize
        })
    }

    pub fn filter_by_line(&self, line: usize) -> Self {
        let line = line as u32;
        Self(
            self.0
                .iter()
                .filter_map(|item| {
                    if item.start.line <= line && item.end.line >= line {
                        Some(item.clone())
                    } else {
                        None
                    }
                })
                .collect(),
        )
    }

    pub fn visual_line(&self, line: usize) -> usize {
        let line = line as u32;
        for folded in &self.0 {
            if line <= folded.start.line {
                return line as usize;
            } else if folded.start.line < line && line <= folded.end.line {
                return folded.start.line as usize;
            }
        }
        line as usize
    }

    /// ??line: 该行是否被折叠。
    /// start_index: 下次检查的起始点
    pub fn contain_line(&self, start_index: usize, line: u32) -> (bool, usize) {
        if start_index >= self.0.len() {
            return (false, start_index);
        }
        let mut last_index = start_index;
        for range in self.0[start_index..].iter() {
            if range.start.line >= line {
                return (false, last_index);
                // todo range.end.line >= line
            } else if range.start.line < line && range.end.line >= line {
                return (true, last_index);
            } else if range.end.line < line {
                last_index += 1;
            }
        }
        (false, last_index)
    }

    pub fn contain_position(&self, position: Position) -> bool {
        self.0
            .iter()
            .any(|x| x.start <= position && x.end >= position)
    }

    pub fn update_status(&self, folding: &mut FoldingRange) {
        if self
            .0
            .iter()
            .any(|x| x.start == folding.start && x.end == folding.end)
        {
            folding.status = FoldingRangeStatus::Fold
        }
    }

    pub fn into_phantom_text(
        self,
        buffer: &Buffer,
        // config: &LapceConfig,
        line: usize,
        inlay_hint_font_size: usize,
        inlay_hint_foreground: Color,
        inlay_hint_background: Color,
    ) -> Vec<PhantomText> {
        self.0
            .into_iter()
            .filter_map(|x| {
                match x.into_phantom_text(
                    buffer,
                    line as u32,
                    inlay_hint_font_size,
                    inlay_hint_foreground,
                    inlay_hint_background,
                ) {
                    Ok(rs) => rs,
                    Err(err) => {
                        error!("{err}");
                        None
                    },
                }
            })
            .collect()
    }
}

fn get_offset(buffer: &Buffer, positon: Position) -> Result<usize> {
    Ok(buffer.offset_of_line(positon.line as usize)? + positon.character as usize)
}

#[derive(Debug, Clone)]
pub struct FoldedRange {
    pub range: Interval,
}

impl FoldedRange {
    pub fn into_phantom_text(
        self,
        buffer: &Buffer,
        // config: &LapceConfig,
        line: usize,
        inlay_hint_font_size: usize,
        inlay_hint_foreground: Color,
        inlay_hint_background: Color,
    ) -> Result<Option<PhantomText>> {
        let start_line = buffer.line_of_offset(self.range.start);
        let end_line = buffer.line_of_offset(self.range.end - 1);

        let folded = buffer.offset_of_line(end_line)?;
        let current = buffer.offset_of_line(start_line + 1)?;
        // info!("line={line} start={:?} end={:?}", self.start,
        // self.end);
        let same_line = end_line == start_line;

        Ok(if start_line == line {
            let Some(start_char) = buffer.char_at_offset(self.range.start) else {
                return Ok(None);
            };
            let Some(end_char) = buffer.char_at_offset(self.range.end - 1) else {
                return Ok(None);
            };

            let folded = buffer.offset_of_line(end_line)?;
            let mut text = String::new();
            text.push(start_char);
            text.push_str("...");
            text.push(end_char);
            let next_line = if same_line {
                None
            } else {
                Some(end_line as usize)
            };
            let (all_len, len) = if same_line {
                (self.range.size(), self.range.size())
            } else {
                (
                    self.range.size(),
                    (self.range.end - folded) + (current - self.range.start),
                )
            };
            Some(PhantomText {
                kind: PhantomTextKind::LineFoldedRang {
                    next_line,
                    len,
                    all_len,
                    start_position: self.start,
                },
                col: start,
                text,
                fg: Some(inlay_hint_foreground),
                font_size: Some(inlay_hint_font_size),
                bg: Some(inlay_hint_background),
                under_line: None,
                final_col: start,
                line: line as usize,
                visual_merge_col: start,
                origin_merge_col: start,
            })
        } else if end_line == line {
            let text = String::new();
            Some(PhantomText {
                kind: PhantomTextKind::LineFoldedRang {
                    next_line:      None,
                    len:            self.end.character as usize,
                    all_len:        self.end.character as usize,
                    start_position: self.start,
                },
                col: 0,
                text,
                fg: None,
                font_size: None,
                bg: None,
                under_line: None,
                final_col: 0,
                line: line as usize,
                visual_merge_col: 0,
                origin_merge_col: 0,
            })
        } else {
            None
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FoldingRange {
    pub start:          Position,
    pub end:            Position,
    pub status:         FoldingRangeStatus,
    pub collapsed_text: Option<String>,
}

impl FoldingRange {
    pub fn from_lsp(value: lsp_types::FoldingRange) -> Self {
        let lsp_types::FoldingRange {
            start_line,
            start_character,
            end_line,
            end_character,
            collapsed_text,
            ..
        } = value;
        let status = FoldingRangeStatus::Unfold;
        Self {
            start: Position {
                line:      start_line,
                character: start_character.unwrap_or_default(),
            },
            end: Position {
                line:      end_line,
                character: end_character.unwrap_or_default(),
            },
            status,
            collapsed_text,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Hash, Copy, Serialize, Deserialize)]
pub struct FoldingPosition {
    pub line:      u32,
    pub character: Option<u32>, // pub kind: Option<FoldingRangeKind>,
}

#[derive(Debug, Clone, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum FoldingRangeStatus {
    Fold,
    #[default]
    Unfold,
}

impl FoldingRangeStatus {
    pub fn click(&mut self) {
        match self {
            FoldingRangeStatus::Fold => {
                *self = FoldingRangeStatus::Unfold;
            },
            FoldingRangeStatus::Unfold => {
                *self = FoldingRangeStatus::Fold;
            },
        }
    }

    pub fn is_folded(&self) -> bool {
        *self == Self::Fold
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct FoldingDisplayItem {
    pub position: Position,
    pub y:        i32,
    pub ty:       FoldingDisplayType,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum FoldingDisplayType {
    UnfoldStart,
    Folded,
    UnfoldEnd,
}

// impl FoldingDisplayItem {
//     pub fn position(&self) -> FoldingPosition {
//         self.position
//     }
// }

#[derive(Debug, Eq, PartialEq, Deserialize, Serialize, Clone, Hash, Copy)]
pub enum FoldingRangeKind {
    Comment,
    Imports,
    Region,
}

impl From<lsp_types::FoldingRangeKind> for FoldingRangeKind {
    fn from(value: lsp_types::FoldingRangeKind) -> Self {
        match value {
            lsp_types::FoldingRangeKind::Comment => FoldingRangeKind::Comment,
            lsp_types::FoldingRangeKind::Imports => FoldingRangeKind::Imports,
            lsp_types::FoldingRangeKind::Region => FoldingRangeKind::Region,
        }
    }
}
