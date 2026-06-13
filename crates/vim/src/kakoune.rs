//! Kakoune mode.
//!
//! Like Helix mode (which was inspired by Kakoune), this is a selection-first
//! editing mode built on top of the vim infrastructure. Unlike Helix, Kakoune
//! has no select mode: selections are extended per-keystroke with
//! Shift-modified movement keys instead.

use std::collections::HashSet;
use std::ops::Range;

use editor::display_map::{DisplaySnapshot, ToDisplayPoint};
use editor::{Anchor, Editor, EditorSettings, MultiBufferOffset, ToOffset, movement};
use gpui::{Action, Context, TaskExt, Window, actions};
use language::{CharClassifier, CharKind, Point};
use multi_buffer::MultiBufferRow;
use schemars::JsonSchema;
use search::{BufferSearchBar, SearchOptions};
use serde::Deserialize;
use settings::Settings;
use text::{Bias, LineEnding, SelectionGoal};
use workspace::searchable::{Direction, FilteredSearchRange};

use text::Selection;

use crate::{
    Vim,
    motion::Motion,
    object::Object,
    state::{KakouneHooksPhase, KakouneObjectTarget, KakouneRegexOp, Mode, Operator, SearchState},
};

actions!(
    vim,
    [
        /// Sets the direction of each selection to forward (cursor after anchor).
        KakouneEnsureForward,
        /// Expands selections to contain full lines, including the trailing
        /// end-of-line.
        KakouneExpandToLines,
        /// Trims selections to only contain full lines, excluding the last
        /// end-of-line.
        KakouneTrimToLines,
        /// Adds a new selection with the next match for the current search query.
        KakouneAddSelectionNext,
        /// Adds a new selection with the previous match for the current search query.
        KakouneAddSelectionPrevious,
        /// Adds an empty line below the cursor without entering insert mode.
        KakouneAddLineBelow,
        /// Adds an empty line above the cursor without entering insert mode.
        KakouneAddLineAbove,
        /// Splits the selections on the matches of a prompted regex.
        KakouneSplitSelections,
        /// Keeps only the selections matching a prompted regex.
        KakouneKeepMatching,
        /// Drops the selections matching a prompted regex.
        KakouneClearMatching,
        /// Selects the first and last characters of each selection.
        KakouneSelectBoundaryChars,
        /// Clears the main selection, keeping the others.
        KakouneClearMainSelection,
        /// Unselects whitespace surrounding each selection, dropping
        /// whitespace-only selections.
        KakouneTrimWhitespace,
        /// Merges contiguous selections together.
        KakouneMergeContiguous,
        /// Saves the current selections so they can be restored later.
        KakouneSaveSelections,
        /// Restores the previously saved selections.
        KakouneRestoreSelections,
        /// Aligns the selections by inserting spaces before their first
        /// characters.
        KakouneAlign,
        /// Copies the indentation of the main selection (or the count one) to
        /// all selected lines.
        KakouneCopyIndent,
        /// Joins the selected lines and selects the spaces inserted in place
        /// of the line breaks.
        KakouneJoinSelectSpaces,
        /// Indents the selected lines, including empty ones.
        KakouneIndentIncludingEmpty,
        /// Unindents the selected lines, keeping incomplete indentation.
        KakouneOutdentKeepIncomplete,
        /// Enters the lock view mode, where view keys can be repeated until
        /// escape.
        PushKakouneView,
        /// Undoes the last selection change.
        KakouneSelectionUndo,
        /// Redoes the last selection change.
        KakouneSelectionRedo,
        /// Disables hooks (automatic behaviors like autoindent and format on
        /// save) for the next command.
        KakouneDisableHooks,
        /// Toggles automatic completion on input (Kakoune's insert-mode
        /// `ctrl-o`).
        KakouneToggleCompletions,
        /// Selects the number under the cursor during an object selection.
        KakouneNumberObject,
        /// Selects the whitespace under the cursor during an object selection.
        KakouneWhitespaceObject,
        /// Saves the active item without formatting it.
        KakouneSaveWithoutFormat,
    ]
);

/// Rotates which selection is the main one.
#[derive(Clone, Deserialize, JsonSchema, PartialEq, Action)]
#[action(namespace = vim)]
#[serde(deny_unknown_fields)]
pub struct KakouneRotateMain {
    #[serde(default)]
    backward: bool,
}

/// Sets the search pattern from the selections (Kakoune's `*`/`alt-*`).
#[derive(Clone, Deserialize, JsonSchema, PartialEq, Action)]
#[action(namespace = vim)]
#[serde(deny_unknown_fields)]
pub struct KakouneSearchPattern {
    /// Wrap word-boundary `\b` assertions around selections that sit on word
    /// boundaries (`*`); when false the pattern is verbatim (`alt-*`).
    #[serde(default)]
    smart: bool,
}

/// Rotates the contents of the selections, in groups of `count` selections
/// when a count is given.
#[derive(Clone, Deserialize, JsonSchema, PartialEq, Action)]
#[action(namespace = vim)]
#[serde(deny_unknown_fields)]
pub struct KakouneRotateContent {
    #[serde(default)]
    backward: bool,
}

/// Combines the saved selections with the current ones (Kakoune's
/// `alt-z`/`alt-Z` menus). With `save`, the result is written back into the
/// saved slot instead of the editor.
#[derive(Clone, Deserialize, JsonSchema, PartialEq, Action)]
#[action(namespace = vim)]
#[serde(deny_unknown_fields)]
pub struct KakouneCombineSelections {
    kind: CombineKind,
    #[serde(default)]
    save: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
enum CombineKind {
    Append,
    Union,
    Intersect,
    SelectLeftmost,
    SelectRightmost,
    SelectLongest,
    SelectShortest,
}

/// Adjusts the horizontal scroll position (the view menu's `m`, `h`, `l`,
/// `<`, and `>` keys).
#[derive(Clone, Deserialize, JsonSchema, PartialEq, Action)]
#[action(namespace = vim)]
#[serde(deny_unknown_fields)]
pub struct KakouneScrollView {
    kind: ScrollViewKind,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ScrollViewKind {
    /// Center the main cursor horizontally.
    Center,
    /// Scroll the window `count` columns left.
    Left,
    /// Scroll the window `count` columns right.
    Right,
    /// Put the main cursor on the leftmost column of the window.
    Leftmost,
    /// Put the main cursor on the rightmost column of the window.
    Rightmost,
}

/// Pastes every yanked selection at each selection and selects each pasted
/// string (Kakoune's `alt-p`/`alt-P`/`alt-R`).
#[derive(Clone, Deserialize, JsonSchema, PartialEq, Action)]
#[action(namespace = vim)]
#[serde(deny_unknown_fields)]
pub struct KakounePasteAll {
    #[serde(default)]
    position: PasteAllPosition,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
enum PasteAllPosition {
    /// Paste after the end of each selection.
    #[default]
    After,
    /// Paste before the start of each selection.
    Before,
    /// Replace each selection with the pasted text.
    Replace,
}

/// Moves the cursor in Kakoune mode, either replacing or extending the
/// current selection.
#[derive(Clone, Deserialize, JsonSchema, PartialEq, Action)]
#[action(namespace = vim)]
#[serde(deny_unknown_fields)]
pub struct KakouneMotion {
    motion: KakouneMotionKind,
    /// Operate on WORDs (any non-whitespace sequence) instead of words.
    #[serde(default)]
    ignore_punctuation: bool,
    /// Extend the current selection instead of replacing it.
    #[serde(default)]
    extend: bool,
}

/// Selects to the next (or previous) sequence enclosed by matching pair
/// characters, mirroring Kakoune's `m`/`M`/`alt-m`/`alt-M`.
#[derive(Clone, Deserialize, JsonSchema, PartialEq, Action)]
#[action(namespace = vim)]
#[serde(deny_unknown_fields)]
pub struct KakouneMatching {
    #[serde(default)]
    backward: bool,
    #[serde(default)]
    extend: bool,
}

/// Starts an object selection: without `to`, the whole surrounding object is
/// selected; with `to`, the selection goes from the cursor to the object's
/// start or end (optionally extending the current selection). With `nested`,
/// every occurrence of the object within each selection is selected instead.
#[derive(Clone, Deserialize, JsonSchema, PartialEq, Action)]
#[action(namespace = vim)]
#[serde(deny_unknown_fields)]
pub struct PushKakouneObject {
    #[serde(default)]
    around: bool,
    #[serde(default)]
    to: Option<ObjectBound>,
    #[serde(default)]
    extend: bool,
    #[serde(default)]
    nested: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ObjectBound {
    Start,
    End,
}

#[derive(Clone, Copy, Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(rename_all = "snake_case")]
enum KakouneMotionKind {
    Left,
    Right,
    Up,
    Down,
    NextWordStart,
    PreviousWordStart,
    NextWordEnd,
    LineBegin,
    LineEnd,
    FirstNonBlank,
    StartOfDocument,
    EndOfDocument,
    WindowTop,
    WindowMiddle,
    WindowBottom,
    SelectToLineBegin,
    SelectToLineEnd,
    EndOfBuffer,
}

impl KakouneMotionKind {
    fn to_motion(self, ignore_punctuation: bool) -> Motion {
        match self {
            Self::Left => Motion::Left,
            Self::Right => Motion::Right,
            Self::Up => Motion::Up {
                display_lines: true,
            },
            Self::Down => Motion::Down {
                display_lines: true,
            },
            Self::NextWordStart => Motion::NextWordStart { ignore_punctuation },
            Self::PreviousWordStart => Motion::PreviousWordStart { ignore_punctuation },
            Self::NextWordEnd => Motion::NextWordEnd { ignore_punctuation },
            Self::LineBegin | Self::SelectToLineBegin => Motion::StartOfLine {
                display_lines: false,
            },
            Self::LineEnd | Self::SelectToLineEnd => Motion::EndOfLine {
                display_lines: false,
            },
            Self::FirstNonBlank => Motion::FirstNonWhitespace {
                display_lines: false,
            },
            Self::StartOfDocument => Motion::StartOfDocument,
            Self::EndOfDocument => Motion::EndOfDocument,
            Self::WindowTop => Motion::WindowTop,
            Self::WindowMiddle => Motion::WindowMiddle,
            Self::WindowBottom => Motion::WindowBottom,
            // EndOfBuffer is handled by `kakoune_end_of_buffer` and never
            // converted; EndOfDocument is its closest equivalent.
            Self::EndOfBuffer => Motion::EndOfDocument,
        }
    }

    /// Kakoune motions that replace each selection with the range from the
    /// cursor to the target, instead of collapsing to the target.
    fn selects_to_target(self) -> bool {
        matches!(self, Self::SelectToLineBegin | Self::SelectToLineEnd)
    }
}

pub fn register(editor: &mut Editor, cx: &mut Context<Vim>) {
    Vim::action(editor, cx, |vim, action: &KakouneMotion, window, cx| {
        let times = Vim::take_count(cx);
        Vim::take_forced_motion(cx);
        if action.motion == KakouneMotionKind::EndOfBuffer {
            vim.kakoune_end_of_buffer(action.extend, window, cx);
            return;
        }
        let motion = action.motion.to_motion(action.ignore_punctuation);
        if !action.extend && action.motion.selects_to_target() {
            vim.kakoune_select_to(motion, times, window, cx);
        } else {
            vim.kakoune_motion(motion, times, action.extend, window, cx);
        }
    });
    Vim::action(editor, cx, |vim, action: &KakouneMatching, window, cx| {
        vim.kakoune_matching(action.backward, action.extend, window, cx);
    });
    Vim::action(editor, cx, |vim, _: &KakouneEnsureForward, window, cx| {
        vim.update_editor(cx, |_, editor, cx| {
            editor.change_selections(Default::default(), window, cx, |s| {
                s.move_with(&mut |_, selection| {
                    selection.reversed = false;
                });
            });
        });
    });
    Vim::action(editor, cx, |vim, _: &KakouneExpandToLines, window, cx| {
        vim.kakoune_expand_to_lines(window, cx);
    });
    Vim::action(editor, cx, |vim, _: &KakouneTrimToLines, window, cx| {
        vim.kakoune_trim_to_lines(window, cx);
    });
    Vim::action(editor, cx, |vim, action: &PushKakouneObject, window, cx| {
        if action.nested {
            vim.push_operator(
                Operator::KakouneNestedObject {
                    around: action.around,
                },
                window,
                cx,
            );
            return;
        }
        let target = match action.to {
            None => KakouneObjectTarget::Whole,
            Some(ObjectBound::Start) => KakouneObjectTarget::ToStart {
                extend: action.extend,
            },
            Some(ObjectBound::End) => KakouneObjectTarget::ToEnd {
                extend: action.extend,
            },
        };
        vim.push_operator(
            Operator::KakouneObject {
                around: action.around,
                target,
            },
            window,
            cx,
        );
    });
    Vim::action(editor, cx, |vim, _: &KakouneAddLineBelow, window, cx| {
        vim.kakoune_add_line(false, window, cx);
    });
    Vim::action(editor, cx, |vim, _: &KakouneAddLineAbove, window, cx| {
        vim.kakoune_add_line(true, window, cx);
    });
    Vim::action(editor, cx, |vim, _: &KakouneSplitSelections, window, cx| {
        vim.kakoune_regex_prompt(KakouneRegexOp::Split, window, cx);
    });
    Vim::action(editor, cx, |vim, _: &KakouneKeepMatching, window, cx| {
        vim.kakoune_regex_prompt(KakouneRegexOp::KeepMatching, window, cx);
    });
    Vim::action(editor, cx, |vim, _: &KakouneClearMatching, window, cx| {
        vim.kakoune_regex_prompt(KakouneRegexOp::ClearMatching, window, cx);
    });
    Vim::action(
        editor,
        cx,
        |vim, _: &KakouneSelectBoundaryChars, window, cx| {
            vim.kakoune_select_boundary_chars(window, cx);
        },
    );
    Vim::action(
        editor,
        cx,
        |vim, _: &KakouneClearMainSelection, window, cx| {
            vim.kakoune_clear_main_selection(window, cx);
        },
    );
    Vim::action(editor, cx, |vim, _: &KakouneTrimWhitespace, window, cx| {
        vim.kakoune_trim_whitespace(window, cx);
    });
    Vim::action(editor, cx, |vim, _: &KakouneMergeContiguous, window, cx| {
        vim.kakoune_merge_contiguous(window, cx);
    });
    Vim::action(editor, cx, |vim, action: &KakouneRotateMain, window, cx| {
        vim.kakoune_rotate_main(action.backward, window, cx);
    });
    Vim::action(
        editor,
        cx,
        |vim, action: &KakouneSearchPattern, window, cx| {
            vim.kakoune_search_pattern(action.smart, window, cx);
        },
    );
    Vim::action(
        editor,
        cx,
        |vim, action: &KakouneRotateContent, window, cx| {
            vim.kakoune_rotate_content(action.backward, window, cx);
        },
    );
    Vim::action(editor, cx, |vim, action: &KakounePasteAll, window, cx| {
        vim.kakoune_paste_all(action.position, window, cx);
    });
    Vim::action(editor, cx, |vim, _: &KakouneSaveSelections, _, cx| {
        vim.update_editor(cx, |vim, editor, _| {
            vim.kakoune_saved_selections = editor.selections.disjoint_anchors().to_vec();
        });
    });
    Vim::action(
        editor,
        cx,
        |vim, _: &KakouneRestoreSelections, window, cx| {
            vim.update_editor(cx, |vim, editor, cx| {
                if vim.kakoune_saved_selections.is_empty() {
                    return;
                }
                let saved = vim.kakoune_saved_selections.clone();
                editor.change_selections(Default::default(), window, cx, |s| {
                    s.select_anchors(saved);
                });
            });
        },
    );
    Vim::action(editor, cx, |vim, _: &KakouneAlign, window, cx| {
        vim.kakoune_align(window, cx);
    });
    Vim::action(editor, cx, |vim, _: &KakouneCopyIndent, window, cx| {
        vim.kakoune_copy_indent(window, cx);
    });
    Vim::action(
        editor,
        cx,
        |vim, _: &KakouneJoinSelectSpaces, window, cx| {
            vim.kakoune_join_select_spaces(window, cx);
        },
    );
    Vim::action(
        editor,
        cx,
        |vim, _: &KakouneIndentIncludingEmpty, window, cx| {
            vim.kakoune_indent_including_empty(window, cx);
        },
    );
    Vim::action(
        editor,
        cx,
        |vim, _: &KakouneOutdentKeepIncomplete, window, cx| {
            vim.kakoune_outdent_keep_incomplete(window, cx);
        },
    );
    Vim::action(editor, cx, |vim, _: &PushKakouneView, window, cx| {
        vim.clear_operator(window, cx);
        vim.push_operator(Operator::KakouneView, window, cx);
    });
    Vim::action(editor, cx, |vim, action: &KakouneScrollView, window, cx| {
        vim.kakoune_scroll_view(action.kind, window, cx);
    });
    Vim::action(
        editor,
        cx,
        |vim, action: &KakouneCombineSelections, window, cx| {
            vim.kakoune_combine_selections(action.kind, action.save, window, cx);
        },
    );
    Vim::action(editor, cx, |vim, _: &KakouneNumberObject, window, cx| {
        let operator = vim.active_operator();
        vim.clear_operator(window, cx);
        match operator {
            Some(Operator::KakouneObject { around, target }) => vim.kakoune_scanned_object(
                target,
                move |map, cursor| number_range(map, cursor, around),
                window,
                cx,
            ),
            Some(Operator::KakouneNestedObject { around }) => {
                vim.kakoune_nested_object(NestedObjectKind::Number, around, window, cx)
            }
            _ => {}
        }
    });
    Vim::action(
        editor,
        cx,
        |vim, _: &KakouneWhitespaceObject, window, cx| {
            let operator = vim.active_operator();
            vim.clear_operator(window, cx);
            match operator {
                Some(Operator::KakouneObject { around, target }) => vim.kakoune_scanned_object(
                    target,
                    move |map, cursor| whitespace_range(map, cursor, around),
                    window,
                    cx,
                ),
                Some(Operator::KakouneNestedObject { around }) => {
                    vim.kakoune_nested_object(NestedObjectKind::Whitespace, around, window, cx)
                }
                _ => {}
            }
        },
    );
    Vim::action(
        editor,
        cx,
        |vim, _: &KakouneToggleCompletions, _, cx| {
            vim.update_editor(cx, |_, editor, cx| {
                let enabled = editor.show_completions_on_input(cx);
                editor.set_show_completions_on_input(Some(!enabled));
            });
        },
    );
    Vim::action(editor, cx, |vim, _: &KakouneDisableHooks, _, cx| {
        vim.kakoune_hooks_disabled = Some(KakouneHooksPhase::Armed);
        vim.status_label = Some("no hooks".into());
        cx.notify();
    });
    Vim::action(
        editor,
        cx,
        |vim, _: &KakouneSaveWithoutFormat, window, cx| {
            vim.kakoune_hooks_disabled = None;
            window.dispatch_action(workspace::SaveWithoutFormat.boxed_clone(), cx);
        },
    );
    Vim::action(editor, cx, |vim, _: &KakouneSelectionUndo, window, cx| {
        vim.kakoune_selection_history_step(false, window, cx);
    });
    Vim::action(editor, cx, |vim, _: &KakouneSelectionRedo, window, cx| {
        vim.kakoune_selection_history_step(true, window, cx);
    });
    Vim::action(editor, cx, |vim, _: &KakouneAddSelectionNext, window, cx| {
        vim.do_helix_select(Direction::Next, true, window, cx);
    });
    Vim::action(
        editor,
        cx,
        |vim, _: &KakouneAddSelectionPrevious, window, cx| {
            vim.do_helix_select(Direction::Prev, true, window, cx);
        },
    );
}

const MATCHING_PAIRS: &[(char, char)] = &[('(', ')'), ('{', '}'), ('[', ']'), ('<', '>')];

fn matching_pair(c: char) -> Option<(char, char, bool)> {
    MATCHING_PAIRS.iter().find_map(|&(open, close)| {
        if c == open {
            Some((open, close, true))
        } else if c == close {
            Some((open, close, false))
        } else {
            None
        }
    })
}

/// Kakoune's `select_matching`: scan from the cursor (inclusively) for the
/// first matching-pair character, then return the range it encloses as
/// `(anchor_char_start, cursor_char_start)`.
fn matching_range(
    map: &DisplaySnapshot,
    cursor: MultiBufferOffset,
    backward: bool,
) -> Option<(MultiBufferOffset, MultiBufferOffset)> {
    let char_at_cursor = movement::chars_after(map, cursor)
        .next()
        .filter(|(c, _)| matching_pair(*c).is_some())
        .map(|(c, range)| (c, range.start));
    let (found_char, found_offset) = char_at_cursor.or_else(|| {
        if backward {
            movement::chars_before(map, cursor)
                .find(|(c, _)| matching_pair(*c).is_some())
                .map(|(c, range)| (c, range.start))
        } else {
            movement::chars_after(map, cursor)
                .find(|(c, _)| matching_pair(*c).is_some())
                .map(|(c, range)| (c, range.start))
        }
    })?;

    let (open, close, is_opener) = matching_pair(found_char)?;
    if is_opener {
        let mut level = 0i32;
        for (c, range) in movement::chars_after(map, found_offset) {
            if c == open {
                level += 1;
            } else if c == close {
                level -= 1;
                if level == 0 {
                    return Some((found_offset, range.start));
                }
            }
        }
        None
    } else {
        // The scan starts on the closing character itself.
        let mut level = 1i32;
        for (c, range) in movement::chars_before(map, found_offset) {
            if c == close {
                level += 1;
            } else if c == open {
                level -= 1;
                if level == 0 {
                    return Some((found_offset, range.start));
                }
            }
        }
        None
    }
}

fn char_at(map: &DisplaySnapshot, offset: MultiBufferOffset) -> Option<char> {
    movement::chars_after(map, offset).next().map(|(c, _)| c)
}

/// Escapes the regex metacharacters Kakoune escapes when turning a selection
/// into a search pattern.
fn regex_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if "^$\\.*+?()[]{}|".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

fn previous_char_start(
    map: &DisplaySnapshot,
    offset: MultiBufferOffset,
) -> Option<MultiBufferOffset> {
    movement::chars_before(map, offset)
        .next()
        .map(|(_, range)| range.start)
}

fn next_char_start(map: &DisplaySnapshot, offset: MultiBufferOffset) -> Option<MultiBufferOffset> {
    movement::chars_after(map, offset)
        .next()
        .map(|(_, range)| range.end)
}

/// Kakoune's number object: digits when inner; `around` also takes `.` and a
/// leading `-`. Returns the range covering the number under the cursor.
fn number_range(
    map: &DisplaySnapshot,
    cursor: MultiBufferOffset,
    around: bool,
) -> Option<Range<MultiBufferOffset>> {
    let is_number = |c: char| c.is_ascii_digit() || (around && c == '.');

    let current = char_at(map, cursor)?;
    if !is_number(current) && current != '-' {
        return None;
    }

    let mut start = cursor;
    while let Some(previous) = previous_char_start(map, start)
        && let Some(c) = char_at(map, previous)
        && is_number(c)
    {
        start = previous;
    }
    // A leading minus is part of the number for both inner and whole.
    if let Some(previous) = previous_char_start(map, start)
        && char_at(map, previous) == Some('-')
    {
        start = previous;
    }

    let mut end = cursor;
    if char_at(map, end) == Some('-') {
        end = next_char_start(map, end)?;
    }
    while let Some(c) = char_at(map, end)
        && is_number(c)
    {
        end = next_char_start(map, end)?;
    }

    (start < end).then_some(start..end)
}

/// Kakoune's whitespace object: spaces and tabs when inner; `around` also
/// crosses newlines.
fn whitespace_range(
    map: &DisplaySnapshot,
    cursor: MultiBufferOffset,
    around: bool,
) -> Option<Range<MultiBufferOffset>> {
    let is_whitespace = |c: char| c == ' ' || c == '\t' || (around && c == '\n');

    if !is_whitespace(char_at(map, cursor)?) {
        return None;
    }

    let mut start = cursor;
    while let Some(previous) = previous_char_start(map, start)
        && let Some(c) = char_at(map, previous)
        && is_whitespace(c)
    {
        start = previous;
    }
    let mut end = cursor;
    while let Some(c) = char_at(map, end)
        && is_whitespace(c)
    {
        end = next_char_start(map, end)?;
    }

    (start < end).then_some(start..end)
}

/// The object kinds supported by the nested selections of `alt-A`/`alt-I`.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum NestedObjectKind {
    Word { ignore_punctuation: bool },
    Number,
    Whitespace,
    Delimiter(char),
    Pair { open: char, close: char, depth: usize },
}

/// Collects every occurrence of the object within `range`, mirroring
/// kakoune's `select_nested_*` and `regex_select_nested` selectors.
fn nested_ranges(
    map: &DisplaySnapshot,
    range: Range<MultiBufferOffset>,
    kind: NestedObjectKind,
    around: bool,
    out: &mut Vec<Range<MultiBufferOffset>>,
) {
    let chars = movement::chars_after(map, range.start)
        .take_while(|(_, char_range)| char_range.end <= range.end);
    match kind {
        NestedObjectKind::Word { ignore_punctuation } => {
            let classifier = map.buffer_snapshot().char_classifier_at(range.start);
            let mut words = Vec::new();
            let mut run: Option<Range<MultiBufferOffset>> = None;
            for (c, char_range) in chars {
                if classifier.kind_with(c, ignore_punctuation) == CharKind::Word {
                    match &mut run {
                        Some(run) => run.end = char_range.end,
                        None => run = Some(char_range),
                    }
                } else if let Some(run) = run.take() {
                    words.push(run);
                }
            }
            words.extend(run);
            for mut word in words {
                if around {
                    // A whole word includes its trailing horizontal blanks.
                    for (c, char_range) in movement::chars_after(map, word.end) {
                        if char_range.end > range.end || (c != ' ' && c != '\t') {
                            break;
                        }
                        word.end = char_range.end;
                    }
                }
                out.push(word);
            }
        }
        NestedObjectKind::Number => {
            let is_number = |c: char| c.is_ascii_digit() || (around && c == '.');
            let mut run: Option<(Range<MultiBufferOffset>, bool)> = None;
            for (c, char_range) in chars {
                let continues = match &run {
                    // A minus only starts a number.
                    Some(_) => is_number(c),
                    None => is_number(c) || c == '-',
                };
                if continues {
                    match &mut run {
                        Some((run, has_digit)) => {
                            run.end = char_range.end;
                            *has_digit |= c.is_ascii_digit();
                        }
                        None => run = Some((char_range, c.is_ascii_digit())),
                    }
                } else if let Some((run, has_digit)) = run.take()
                    && has_digit
                {
                    out.push(run);
                }
            }
            if let Some((run, true)) = run {
                out.push(run);
            }
        }
        NestedObjectKind::Whitespace => {
            let is_whitespace = |c: char| c == ' ' || c == '\t' || (around && c == '\n');
            let mut run: Option<Range<MultiBufferOffset>> = None;
            for (c, char_range) in chars {
                if is_whitespace(c) {
                    match &mut run {
                        Some(run) => run.end = char_range.end,
                        None => run = Some(char_range),
                    }
                } else if let Some(run) = run.take() {
                    out.push(run);
                }
            }
            out.extend(run);
        }
        NestedObjectKind::Delimiter(delimiter) => {
            // Occurrences alternate between opening and closing a region.
            let mut start: Option<MultiBufferOffset> = None;
            for (c, char_range) in chars {
                if c != delimiter {
                    continue;
                }
                match start.take() {
                    None => start = Some(if around { char_range.start } else { char_range.end }),
                    Some(start) => {
                        let end = if around { char_range.end } else { char_range.start };
                        if start <= end {
                            out.push(start..end);
                        }
                    }
                }
            }
            if let Some(start) = start {
                out.push(start..range.end);
            }
        }
        NestedObjectKind::Pair { open, close, depth } => {
            let mut level = -(depth as i64) - 1;
            let mut start: Option<MultiBufferOffset> = None;
            for (c, char_range) in chars {
                if c == open {
                    level += 1;
                    if level == 0 {
                        start = Some(if around { char_range.start } else { char_range.end });
                    }
                } else if c == close {
                    if level == 0 && let Some(start) = start.take() {
                        let end = if around { char_range.end } else { char_range.start };
                        if start <= end {
                            out.push(start..end);
                        }
                    }
                    level -= 1;
                }
            }
            if let Some(start) = start {
                out.push(start..range.end);
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum WordTarget {
    NextStart,
    PreviousStart,
    NextEnd,
}

/// Character scanning helpers mirroring Kakoune's `Utf8Iterator`-based word
/// selection primitives. Positions are offsets of a character's first byte.
struct WordScanner<'a> {
    map: &'a DisplaySnapshot,
    classifier: CharClassifier,
    ignore_punctuation: bool,
}

impl WordScanner<'_> {
    fn char_at(&self, offset: MultiBufferOffset) -> Option<char> {
        movement::chars_after(self.map, offset).next().map(|(c, _)| c)
    }

    fn next(&self, offset: MultiBufferOffset) -> Option<MultiBufferOffset> {
        movement::chars_after(self.map, offset)
            .next()
            .map(|(_, range)| range.end)
    }

    fn previous(&self, offset: MultiBufferOffset) -> Option<MultiBufferOffset> {
        movement::chars_before(self.map, offset)
            .next()
            .map(|(_, range)| range.start)
    }

    fn kind(&self, c: char) -> CharKind {
        self.classifier.kind_with(c, self.ignore_punctuation)
    }

    fn is_blank(&self, c: char) -> bool {
        c != '\n' && self.kind(c) == CharKind::Whitespace
    }

    /// Kakoune moves the selection anchor onto the next/previous character
    /// when the cursor sits on a category boundary, then skips end-of-line
    /// characters.
    fn adjust_begin(
        &self,
        cursor: MultiBufferOffset,
        towards_previous: bool,
    ) -> Option<MultiBufferOffset> {
        let mut begin = cursor;
        let current = self.char_at(begin)?;
        let neighbor_begin = if towards_previous {
            self.previous(begin)?
        } else {
            self.next(begin)?
        };
        let neighbor = self.char_at(neighbor_begin)?;
        if self.kind(current) != self.kind(neighbor) {
            begin = neighbor_begin;
        }
        while self.char_at(begin)? == '\n' {
            begin = if towards_previous {
                self.previous(begin)?
            } else {
                self.next(begin)?
            };
        }
        Some(begin)
    }

    /// One application of the Kakoune word selection for `target`, from the
    /// character position `cursor`. Returns the new selection as
    /// `(anchor_char_start, cursor_char_start)`.
    fn select_word(
        &self,
        cursor: MultiBufferOffset,
        target: WordTarget,
    ) -> Option<(MultiBufferOffset, MultiBufferOffset)> {
        let begin = self.adjust_begin(cursor, target == WordTarget::PreviousStart)?;
        match target {
            WordTarget::NextStart => {
                let first = self.char_at(begin)?;
                let mut end = self.next(begin)?;
                if self.kind(first) != CharKind::Whitespace {
                    while let Some(c) = self.char_at(end)
                        && self.kind(c) == self.kind(first)
                    {
                        end = self.next(end)?;
                    }
                }
                while let Some(c) = self.char_at(end)
                    && self.is_blank(c)
                {
                    end = self.next(end)?;
                }
                Some((begin, self.previous(end)?))
            }
            WordTarget::NextEnd => {
                let mut end = begin;
                while let Some(c) = self.char_at(end)
                    && self.is_blank(c)
                {
                    end = self.next(end)?;
                }
                let first = self.char_at(end)?;
                if self.kind(first) != CharKind::Whitespace {
                    while let Some(c) = self.char_at(end)
                        && self.kind(c) == self.kind(first)
                    {
                        end = self.next(end)?;
                    }
                }
                Some((begin, self.previous(end)?))
            }
            WordTarget::PreviousStart => {
                let mut end = begin;
                while let Some(c) = self.char_at(end)
                    && self.is_blank(c)
                    && let Some(previous) = self.previous(end)
                {
                    end = previous;
                }
                let first = self.char_at(end)?;
                if self.kind(first) != CharKind::Whitespace {
                    while let Some(previous) = self.previous(end)
                        && let Some(c) = self.char_at(previous)
                        && self.kind(c) == self.kind(first)
                    {
                        end = previous;
                    }
                }
                Some((begin, end))
            }
        }
    }
}

impl Vim {
    pub fn kakoune_motion(
        &mut self,
        motion: Motion,
        times: Option<usize>,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Motion::ZedSearchResult {
            prior_selections,
            new_selections,
        } = &motion
        {
            self.kakoune_search_result(prior_selections.clone(), new_selections.clone(), window, cx);
            return;
        }
        // Kakoune's goto-line commands (gg, gj, and count-prefixed goto) land
        // on column 0 of the target line rather than preserving the column.
        if matches!(motion, Motion::StartOfDocument | Motion::EndOfDocument) {
            self.kakoune_goto_line(motion, times, extend, window, cx);
            return;
        }
        if extend {
            // Helix select mode extends selections exactly like Kakoune's
            // Shift-modified movements.
            self.helix_select_motion(motion, times, window, cx);
            return;
        }
        match motion {
            Motion::NextWordStart { ignore_punctuation } => self.kakoune_word_motion(
                WordTarget::NextStart,
                times,
                ignore_punctuation,
                window,
                cx,
            ),
            Motion::PreviousWordStart { ignore_punctuation } => self.kakoune_word_motion(
                WordTarget::PreviousStart,
                times,
                ignore_punctuation,
                window,
                cx,
            ),
            Motion::NextWordEnd { ignore_punctuation } => {
                self.kakoune_word_motion(WordTarget::NextEnd, times, ignore_punctuation, window, cx)
            }
            _ => self.helix_move_cursor(motion, times, window, cx),
        }
    }

    fn kakoune_word_motion(
        &mut self,
        target: WordTarget,
        times: Option<usize>,
        ignore_punctuation: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let times = times.unwrap_or(1);
        self.helix_new_selections(window, cx, &mut |cursor, map| {
            let scanner = WordScanner {
                map,
                classifier: map
                    .buffer_snapshot()
                    .char_classifier_at(cursor.to_point(map)),
                ignore_punctuation,
            };
            let mut selection = None;
            let mut cursor = cursor.to_offset(map, Bias::Left);
            for _ in 0..times {
                let Some((anchor, new_cursor)) = scanner.select_word(cursor, target) else {
                    break;
                };
                cursor = new_cursor;
                selection = Some((anchor, new_cursor));
            }
            let (anchor, cursor) = selection?;
            // The selection covers both the anchor and cursor characters, so
            // the exclusive edge sits one character past whichever is on the
            // selection's trailing end.
            let (head, tail) = if cursor < anchor {
                (
                    cursor.to_display_point(map),
                    scanner.next(anchor)?.to_display_point(map),
                )
            } else {
                (
                    scanner.next(cursor)?.to_display_point(map),
                    anchor.to_display_point(map),
                )
            };
            Some((head, tail))
        });
    }

    /// Replaces each selection with the range between the current cursor and
    /// the motion target (used by `alt-h` and `alt-l`).
    ///
    /// The target character itself is not included: a backward target is
    /// always covered by the selection start, and `EndOfLine`'s `move_point`
    /// already lands past the last character.
    fn kakoune_select_to(
        &mut self,
        motion: Motion,
        times: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_editor(cx, |_, editor, cx| {
            let text_layout_details = editor.text_layout_details(window, cx);
            editor.change_selections(Default::default(), window, cx, |s| {
                s.move_with(&mut |map, selection| {
                    let cursor = if selection.reversed || selection.is_empty() {
                        selection.head()
                    } else {
                        movement::left(map, selection.head())
                    };
                    let Some((target, _)) = motion.move_point(
                        map,
                        cursor,
                        selection.goal,
                        times,
                        &text_layout_details,
                    ) else {
                        return;
                    };
                    if target < cursor {
                        // The original cursor character stays selected, so the
                        // anchor sits one character to its right.
                        selection.set_head_tail(
                            target,
                            movement::right(map, cursor),
                            SelectionGoal::None,
                        );
                    } else {
                        selection.set_head_tail(target, cursor, SelectionGoal::None);
                    }
                })
            });
        });
    }

    /// Kakoune's `*`/`alt-*`: build a search pattern from the selections and
    /// set it on the search bar (which `n`/`N` consume), without moving the
    /// cursor or taking focus from the document. With `smart`, selections
    /// that sit on word boundaries are wrapped with `\b`.
    fn kakoune_search_pattern(&mut self, smart: bool, window: &mut Window, cx: &mut Context<Self>) {
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let pattern = self.update_editor(cx, |_, editor, cx| {
            let display_map = editor.display_snapshot(cx);
            let buffer = display_map.buffer_snapshot();
            let mut seen = HashSet::new();
            let mut parts = Vec::new();
            for selection in editor.selections.all::<MultiBufferOffset>(&display_map) {
                if selection.start >= selection.end {
                    continue;
                }
                let text: String = buffer.text_for_range(selection.start..selection.end).collect();
                let mut part = String::new();
                // Beginning-of-word: the first selected char is a word char
                // preceded by a non-word char (or the buffer start).
                let bow = smart
                    && char_at(&display_map, selection.start).is_some_and(is_word)
                    && previous_char_start(&display_map, selection.start)
                        .and_then(|offset| char_at(&display_map, offset))
                        .is_none_or(|c| !is_word(c));
                // End-of-word: the last selected char is a word char followed
                // by a non-word char (or the buffer end).
                let eow = smart
                    && previous_char_start(&display_map, selection.end)
                        .and_then(|offset| char_at(&display_map, offset))
                        .is_some_and(is_word)
                    && char_at(&display_map, selection.end).is_none_or(|c| !is_word(c));
                if bow {
                    part.push_str("\\b");
                }
                part.push_str(&regex_escape(&text));
                if eow {
                    part.push_str("\\b");
                }
                if seen.insert(part.clone()) {
                    parts.push(part);
                }
            }
            parts.join("|")
        });
        let Some(pattern) = pattern.filter(|pattern| !pattern.is_empty()) else {
            return;
        };

        let Some(pane) = self.pane(window, cx) else {
            return;
        };
        pane.update(cx, |pane, cx| {
            let Some(search_bar) = pane.toolbar().read(cx).item_of_type::<BufferSearchBar>() else {
                return;
            };
            search_bar.update(cx, |search_bar, cx| {
                if !search_bar.show(window, cx) {
                    return;
                }
                let mut options = SearchOptions::REGEX;
                if EditorSettings::get_global(cx).search.case_sensitive {
                    options |= SearchOptions::CASE_SENSITIVE;
                }
                // Compute matches but keep focus in the document.
                drop(search_bar.search(&pattern, Some(options), true, window, cx));
                search_bar.focus_editor(&Default::default(), window, cx);
            });
        });
        Vim::globals(cx).registers.insert('/', pattern.into());
    }

    /// `/` selects the next match; `?` extends the current selection up to
    /// the end of the next match.
    fn kakoune_search_result(
        &mut self,
        prior_selections: Vec<std::ops::Range<editor::Anchor>>,
        new_selections: Vec<std::ops::Range<editor::Anchor>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let extend = std::mem::take(&mut self.search.kakoune_extend);
        self.update_editor(cx, |_, editor, cx| {
            editor.change_selections(Default::default(), window, cx, |s| {
                if extend
                    && let (Some(prior), Some(target)) =
                        (prior_selections.last(), new_selections.last())
                {
                    s.select_anchor_ranges([prior.start..target.end]);
                } else {
                    s.select_anchor_ranges(new_selections.clone());
                }
            });
        });
    }

    fn kakoune_goto_line(
        &mut self,
        motion: Motion,
        times: Option<usize>,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_editor(cx, |_, editor, cx| {
            let text_layout_details = editor.text_layout_details(window, cx);
            editor.change_selections(Default::default(), window, cx, |s| {
                s.move_with(&mut |map, selection| {
                    let cursor = if selection.reversed || selection.is_empty() {
                        selection.head()
                    } else {
                        movement::left(map, selection.head())
                    };
                    let Some((point, _)) = motion.move_point(
                        map,
                        cursor,
                        selection.goal,
                        times,
                        &text_layout_details,
                    ) else {
                        return;
                    };
                    let target = movement::line_beginning(map, point, false);
                    if extend {
                        selection.set_head(target, SelectionGoal::None);
                        if !selection.reversed {
                            selection.end = movement::right(map, selection.end);
                        }
                    } else {
                        selection.collapse_to(target, SelectionGoal::None);
                    }
                });
            });
        });
    }

    fn kakoune_end_of_buffer(&mut self, extend: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.update_editor(cx, |_, editor, cx| {
            editor.change_selections(Default::default(), window, cx, |s| {
                s.move_with(&mut |map, selection| {
                    let max = map.max_point();
                    if extend {
                        selection.set_head(max, SelectionGoal::None);
                    } else {
                        // The cursor lands on the buffer's last character.
                        let target = movement::saturating_left(map, max);
                        selection.collapse_to(target, SelectionGoal::None);
                    }
                })
            });
        });
    }

    /// Kakoune's `m`/`M`/`alt-m`/`alt-M`: select (or extend) to the next or
    /// previous sequence enclosed by matching pair characters.
    fn kakoune_matching(
        &mut self,
        backward: bool,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if extend {
            self.update_editor(cx, |_, editor, cx| {
                editor.change_selections(Default::default(), window, cx, |s| {
                    s.move_with(&mut |map, selection| {
                        let cursor = if selection.reversed || selection.is_empty() {
                            selection.head()
                        } else {
                            movement::left(map, selection.head())
                        };
                        let cursor_offset = cursor.to_offset(map, Bias::Left);
                        let Some((_, target)) = matching_range(map, cursor_offset, backward) else {
                            return;
                        };
                        selection.set_head(target.to_display_point(map), SelectionGoal::None);
                        if !selection.reversed {
                            selection.end = movement::right(map, selection.end);
                        }
                    });
                });
            });
        } else {
            self.helix_new_selections(window, cx, &mut |cursor, map| {
                let cursor_offset = cursor.to_offset(map, Bias::Left);
                let (anchor, target) = matching_range(map, cursor_offset, backward)?;
                let next_char = |offset: MultiBufferOffset| {
                    movement::chars_after(map, offset)
                        .next()
                        .map(|(_, range)| range.end)
                };
                let (head, tail) = if target < anchor {
                    (target.to_display_point(map), next_char(anchor)?.to_display_point(map))
                } else {
                    (next_char(target)?.to_display_point(map), anchor.to_display_point(map))
                };
                Some((head, tail))
            });
        }
    }

    /// Kakoune's `[`/`]`/`{`/`}`: select (or extend) from the cursor to the
    /// surrounding object's start or end.
    pub(crate) fn kakoune_select_object_bound(
        &mut self,
        object: Object,
        around: bool,
        to_end: bool,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.stop_recording(cx);
        self.update_editor(cx, |_, editor, cx| {
            editor.change_selections(Default::default(), window, cx, |s| {
                s.move_with(&mut |map, selection| {
                    let Ok(Some(range)) = object.helix_range(map, selection.clone(), around) else {
                        return;
                    };
                    let cursor = if selection.reversed || selection.is_empty() {
                        selection.head()
                    } else {
                        movement::left(map, selection.head())
                    };
                    if extend {
                        let target = if to_end { range.end } else { range.start };
                        selection.set_head(target, SelectionGoal::None);
                    } else if to_end {
                        selection.set_head_tail(range.end, cursor, SelectionGoal::None);
                    } else {
                        // The cursor's character stays selected, so the anchor
                        // sits one character to its right.
                        selection.set_head_tail(
                            range.start,
                            movement::right(map, cursor),
                            SelectionGoal::None,
                        );
                    }
                });
            });
        });
    }

    /// Opens the search prompt for a selection transformation (`S`, `alt-k`,
    /// `alt-K`); the transformation runs when the prompt is submitted.
    fn kakoune_regex_prompt(
        &mut self,
        op: KakouneRegexOp,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        Vim::take_forced_motion(cx);
        let Some(pane) = self.pane(window, cx) else {
            return;
        };
        let prior_selections = self.editor_selections(window, cx);
        pane.update(cx, |pane, cx| {
            if let Some(search_bar) = pane.toolbar().read(cx).item_of_type::<BufferSearchBar>() {
                search_bar.update(cx, |search_bar, cx| {
                    if !search_bar.show(window, cx) {
                        return;
                    }

                    search_bar.select_query(window, cx);
                    cx.focus_self(window);

                    search_bar.set_replacement(None, cx);
                    let mut options = SearchOptions::REGEX;
                    if EditorSettings::get_global(cx).search.case_sensitive {
                        options |= SearchOptions::CASE_SENSITIVE;
                    }
                    search_bar.set_search_options(options, cx);
                    if let Some(search) = search_bar.set_search_within_selection(
                        Some(FilteredSearchRange::Selection),
                        window,
                        cx,
                    ) {
                        cx.spawn_in(window, async move |search_bar, cx| {
                            if search.await.is_ok() {
                                search_bar.update_in(cx, |search_bar, window, cx| {
                                    search_bar.activate_current_match(window, cx)
                                })
                            } else {
                                Ok(())
                            }
                        })
                        .detach_and_log_err(cx);
                    }
                    self.search = SearchState {
                        direction: Direction::Next,
                        count: 1,
                        cmd_f_search: false,
                        prior_selections,
                        prior_operator: self.operator_stack.last().cloned(),
                        prior_mode: self.mode,
                        helix_select: false,
                        kakoune_extend: false,
                        kakoune_regex_op: Some(op),
                        _dismiss_subscription: None,
                    }
                });
            }
        });
        // Show which transformation the prompt drives, like Kakoune's
        // prompts. The label is cleared when the next vim action (such as
        // the search submit) is dispatched.
        self.status_label = Some(
            match op {
                KakouneRegexOp::Split => "split:",
                KakouneRegexOp::KeepMatching => "keep matching:",
                KakouneRegexOp::ClearMatching => "keep not matching:",
            }
            .into(),
        );
        cx.notify();
    }

    /// Applies a pending selection transformation, where the editor's current
    /// selections are the regex matches within the prior selections.
    pub(crate) fn kakoune_apply_regex_op(
        &mut self,
        op: KakouneRegexOp,
        prior_selections: Vec<Range<Anchor>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_editor(cx, |_, editor, cx| {
            let display_map = editor.display_snapshot(cx);
            let buffer_snapshot = display_map.buffer_snapshot();
            let matches: Vec<Range<MultiBufferOffset>> = editor
                .selections
                .all::<MultiBufferOffset>(&display_map)
                .into_iter()
                .map(|selection| selection.start..selection.end)
                .collect();
            let priors: Vec<Range<MultiBufferOffset>> = prior_selections
                .iter()
                .map(|range| range.start.to_offset(buffer_snapshot)..range.end.to_offset(buffer_snapshot))
                .collect();

            let mut new_ranges = Vec::new();
            match op {
                KakouneRegexOp::Split => {
                    for prior in &priors {
                        let mut cursor = prior.start;
                        for matched in matches
                            .iter()
                            .filter(|matched| matched.start >= prior.start && matched.end <= prior.end)
                        {
                            if matched.start > cursor {
                                new_ranges.push(cursor..matched.start);
                            }
                            cursor = cursor.max(matched.end);
                        }
                        if cursor < prior.end {
                            new_ranges.push(cursor..prior.end);
                        }
                    }
                }
                KakouneRegexOp::KeepMatching | KakouneRegexOp::ClearMatching => {
                    let keep = op == KakouneRegexOp::KeepMatching;
                    for prior in &priors {
                        let has_match = matches
                            .iter()
                            .any(|matched| matched.start < prior.end && matched.end > prior.start);
                        if has_match == keep {
                            new_ranges.push(prior.clone());
                        }
                    }
                }
            }
            // Kakoune refuses transformations that would leave no selection.
            if new_ranges.is_empty() {
                new_ranges = priors;
            }

            editor.change_selections(Default::default(), window, cx, |s| {
                s.select_ranges(new_ranges);
            });
        });
    }

    /// Kakoune's `alt-S`: select the first and last characters of each
    /// selection.
    fn kakoune_select_boundary_chars(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_editor(cx, |_, editor, cx| {
            let display_map = editor.display_snapshot(cx);
            let buffer_snapshot = display_map.buffer_snapshot();
            let mut new_ranges = Vec::new();
            for selection in editor.selections.all::<MultiBufferOffset>(&display_map) {
                let (start, end) = (selection.start, selection.end);
                let first_end = match buffer_snapshot.chars_at(start).next() {
                    Some(c) if start + c.len_utf8() < end => start + c.len_utf8(),
                    _ => {
                        new_ranges.push(start..end);
                        continue;
                    }
                };
                new_ranges.push(start..first_end);
                if let Some(c) = buffer_snapshot.reversed_chars_at(end).next() {
                    let last_start = end - c.len_utf8();
                    if last_start >= first_end {
                        new_ranges.push(last_start..end);
                    }
                }
            }
            editor.change_selections(Default::default(), window, cx, |s| {
                s.select_ranges(new_ranges);
            });
        });
    }

    /// Kakoune's `_`: trim surrounding whitespace from each selection and
    /// drop the ones that only contain whitespace.
    fn kakoune_trim_whitespace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_editor(cx, |_, editor, cx| {
            let display_map = editor.display_snapshot(cx);
            let buffer_snapshot = display_map.buffer_snapshot();
            let mut new_ranges = Vec::new();
            for selection in editor.selections.all::<MultiBufferOffset>(&display_map) {
                let mut start = selection.start;
                let mut end = selection.end;
                for c in buffer_snapshot.chars_at(start) {
                    if start >= end || !c.is_whitespace() {
                        break;
                    }
                    start += c.len_utf8();
                }
                for c in buffer_snapshot.reversed_chars_at(end) {
                    if end <= start || !c.is_whitespace() {
                        break;
                    }
                    end -= c.len_utf8();
                }
                if start < end {
                    new_ranges.push(start..end);
                }
            }
            // Kakoune keeps the selections when none would remain.
            if new_ranges.is_empty() {
                return;
            }
            editor.change_selections(Default::default(), window, cx, |s| {
                s.select_ranges(new_ranges);
            });
        });
    }

    /// Kakoune's `alt-_`: merge contiguous selections together.
    fn kakoune_merge_contiguous(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_editor(cx, |_, editor, cx| {
            let display_map = editor.display_snapshot(cx);
            let mut new_ranges: Vec<Range<MultiBufferOffset>> = Vec::new();
            for selection in editor.selections.all::<MultiBufferOffset>(&display_map) {
                if let Some(last) = new_ranges.last_mut()
                    && selection.start <= last.end
                {
                    last.end = last.end.max(selection.end);
                } else {
                    new_ranges.push(selection.start..selection.end);
                }
            }
            editor.change_selections(Default::default(), window, cx, |s| {
                s.select_ranges(new_ranges);
            });
        });
    }

    /// Kakoune's `(`/`)`: rotate which selection is the main one. Zed's main
    /// selection is the newest, so this swaps the newest id onto the next or
    /// previous selection.
    fn kakoune_rotate_main(&mut self, backward: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.update_editor(cx, |_, editor, cx| {
            let display_map = editor.display_snapshot(cx);
            let newest_id = editor.selections.newest_anchor().id;
            let mut selections = editor.selections.all::<MultiBufferOffset>(&display_map);
            if selections.len() <= 1 {
                return;
            }
            let Some(current) = selections.iter().position(|s| s.id == newest_id) else {
                return;
            };
            let target = if backward {
                (current + selections.len() - 1) % selections.len()
            } else {
                (current + 1) % selections.len()
            };
            let target_id = selections[target].id;
            selections[current].id = target_id;
            selections[target].id = newest_id;
            editor.change_selections(Default::default(), window, cx, |s| {
                s.select(selections);
            });
        });
    }

    /// Kakoune's `alt-(`/`alt-)`: rotate the contents of the selections, in
    /// groups of `count` selections when a count is given. The main selection
    /// follows its content.
    fn kakoune_rotate_content(
        &mut self,
        backward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let group_size = Vim::take_count(cx);
        self.update_editor(cx, |_, editor, cx| {
            let display_map = editor.display_snapshot(cx);
            let buffer_snapshot = display_map.buffer_snapshot();
            let newest_id = editor.selections.newest_anchor().id;
            let mut selections = editor.selections.all::<MultiBufferOffset>(&display_map);
            if selections.len() <= 1 {
                return;
            }
            let group = match group_size {
                Some(group) if group <= selections.len() => group,
                _ => selections.len(),
            };

            let mut texts: Vec<String> = selections
                .iter()
                .map(|selection| {
                    buffer_snapshot
                        .text_for_range(selection.start..selection.end)
                        .collect()
                })
                .collect();
            for chunk in texts.chunks_mut(group) {
                if backward {
                    chunk.rotate_left(1);
                } else {
                    chunk.rotate_right(1);
                }
            }

            // The edits are expressed in pre-edit coordinates and applied
            // atomically below.
            let edits: Vec<_> = selections
                .iter()
                .zip(&texts)
                .map(|(selection, text)| (selection.start..selection.end, text.clone()))
                .collect();

            // Recompute the selection ranges to cover the rotated contents.
            let mut delta = 0isize;
            for (selection, text) in selections.iter_mut().zip(&texts) {
                let old_length = (selection.end.0 - selection.start.0) as isize;
                let start = selection.start.0.saturating_add_signed(delta);
                selection.start = MultiBufferOffset(start);
                selection.end = MultiBufferOffset(start + text.len());
                delta += text.len() as isize - old_length;
            }

            // The main selection follows its content into the neighboring
            // slot of its group.
            if let Some(main) = selections.iter().position(|s| s.id == newest_id) {
                let group_start = (main / group) * group;
                let group_length = (group_start + group).min(selections.len()) - group_start;
                let rotated = if backward {
                    (main - group_start + group_length - 1) % group_length
                } else {
                    (main - group_start + 1) % group_length
                };
                let new_main = group_start + rotated;
                if new_main != main {
                    selections[main].id = selections[new_main].id;
                    selections[new_main].id = newest_id;
                }
            }

            editor.transact(window, cx, |editor, window, cx| {
                editor.edit(edits, cx);
                editor.change_selections(Default::default(), window, cx, |s| {
                    s.select(selections);
                });
            });
        });
    }

    /// Kakoune's `alt-p`/`alt-P`/`alt-R`: paste every yanked selection at
    /// each selection (after it, before it, or replacing it) and select each
    /// pasted string.
    fn kakoune_paste_all(
        &mut self,
        position: PasteAllPosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        Vim::take_count(cx);
        Vim::take_forced_motion(cx);
        self.update_editor(cx, |vim, editor, cx| {
            if editor.read_only(cx) {
                return;
            }

            editor.transact(window, cx, |editor, window, cx| {
                editor.set_clip_at_line_ends(false, cx);

                let selected_register = vim.selected_register.take();
                let Some(register) = Vim::update_globals(cx, |globals, cx| {
                    globals.read_register(selected_register, Some(editor), cx)
                })
                .filter(|register| !register.text.is_empty()) else {
                    return;
                };

                // The register text holds one piece per yanked selection,
                // joined by newlines unless a piece is an entire line.
                let mut pieces: Vec<String> = Vec::new();
                if let Some(clipboard_selections) = register
                    .clipboard_selections
                    .as_ref()
                    .filter(|selections| selections.len() > 1)
                {
                    let mut start = 0;
                    for clipboard_selection in clipboard_selections {
                        let end = start + clipboard_selection.len;
                        let Some(piece) = register.text.get(start..end) else {
                            break;
                        };
                        pieces.push(piece.to_string());
                        start = if clipboard_selection.is_entire_line {
                            end
                        } else {
                            end + 1
                        };
                    }
                } else {
                    pieces.push(register.text.to_string());
                }
                pieces.retain(|piece| !piece.is_empty());
                for piece in &mut pieces {
                    LineEnding::normalize(piece);
                }
                if pieces.is_empty() {
                    return;
                }
                let linewise = pieces.iter().all(|piece| piece.ends_with('\n'));
                let all = pieces.concat();

                let display_map = editor.display_snapshot(cx);
                let current_selections = editor.selections.all_adjusted_display(&display_map);

                let mut edits = Vec::new();
                let mut paste_starts = Vec::new();
                for selection in &current_selections {
                    if position == PasteAllPosition::Replace {
                        // Kakoune selections always cover at least one
                        // character, so an empty selection replaces the
                        // character under the cursor.
                        let end = if selection.start == selection.end {
                            movement::right(&display_map, selection.end)
                        } else {
                            selection.end
                        };
                        let range = selection.start.to_point(&display_map)
                            ..end.to_point(&display_map);
                        paste_starts.push((
                            display_map.buffer_snapshot().anchor_before(range.start),
                            0,
                        ));
                        edits.push((range, all.clone()));
                        continue;
                    }

                    let before = position == PasteAllPosition::Before;
                    let mut leading_newline = false;
                    let display_point = if linewise {
                        if before {
                            movement::line_beginning(&display_map, selection.start, false)
                        } else if selection.start == selection.end {
                            let line_end =
                                movement::line_end(&display_map, selection.end, false);
                            let next_line = movement::right(&display_map, line_end);
                            // On the buffer's last line there is no following
                            // line to paste at, so a newline is inserted first.
                            leading_newline = next_line == line_end;
                            next_line
                        } else {
                            selection.end
                        }
                    } else if before {
                        selection.start
                    } else if selection.start == selection.end {
                        // The cursor sits on a character, so pasting after
                        // means after that character — except on an empty
                        // line, where there is no character.
                        let right = movement::right(&display_map, selection.end);
                        if right.row() != selection.end.row() && selection.end.column() == 0 {
                            selection.end
                        } else {
                            right
                        }
                    } else {
                        selection.end
                    };
                    let point = display_point.to_point(&display_map);
                    let text = if leading_newline {
                        format!("\n{all}")
                    } else {
                        all.clone()
                    };
                    paste_starts.push((
                        display_map.buffer_snapshot().anchor_before(point),
                        leading_newline as usize,
                    ));
                    edits.push((point..point, text));
                }

                editor.edit(edits, cx);

                let snapshot = editor.buffer().read(cx).snapshot(cx);
                let mut new_ranges = Vec::new();
                for (paste_start, leading) in paste_starts {
                    let mut offset = paste_start.to_offset(&snapshot) + leading;
                    for piece in &pieces {
                        new_ranges.push(offset..offset + piece.len());
                        offset += piece.len();
                    }
                }
                editor.change_selections(Default::default(), window, cx, |s| {
                    s.select_ranges(new_ranges);
                });
            });
        });
    }

    /// Kakoune's `&`: align the selection cursors by inserting spaces before
    /// the first character of each selection. Multiple selections on the
    /// same line form column groups (the first selection of each line is in
    /// the first group, and so on), each aligned independently in order.
    fn kakoune_align(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_editor(cx, |_, editor, cx| {
            let display_map = editor.display_snapshot(cx);
            let selections = editor.selections.all::<Point>(&display_map);

            // Like Kakoune, alignment only works with single-line selections.
            // A full-line selection's exclusive end sits on the next row.
            let single_line = |selection: &text::Selection<Point>| {
                let end_row = if selection.end.column == 0 && selection.end.row > selection.start.row
                {
                    selection.end.row - 1
                } else {
                    selection.end.row
                };
                selection.start.row == end_row
            };
            if !selections.iter().all(single_line) {
                return;
            }

            let mut groups: Vec<Vec<usize>> = Vec::new();
            let mut last_row = None;
            let mut column = 0;
            for (index, selection) in selections.iter().enumerate() {
                column = if last_row == Some(selection.start.row) {
                    column + 1
                } else {
                    0
                };
                if groups.len() <= column {
                    groups.push(Vec::new());
                }
                groups[column].push(index);
                last_row = Some(selection.start.row);
            }

            editor.transact(window, cx, |editor, _, cx| {
                // Groups are aligned in order: insertions for one group shift
                // the later groups, whose columns are measured afterwards.
                for group in groups {
                    let display_map = editor.display_snapshot(cx);
                    let selections = editor.selections.all::<Point>(&display_map);
                    // The display column expands tabs.
                    let cursor_column = |index: usize| {
                        selections[index].head().to_display_point(&display_map).column()
                    };
                    let Some(max_column) = group.iter().map(|&index| cursor_column(index)).max()
                    else {
                        continue;
                    };
                    let mut edits = Vec::new();
                    for &index in &group {
                        let padding = max_column - cursor_column(index);
                        if padding == 0 {
                            continue;
                        }
                        let start = selections[index].start;
                        // Like kakoune's aligntab option, buffers indented
                        // with hard tabs are padded with tabs up to the last
                        // tabstop, then spaces.
                        let settings = display_map
                            .buffer_snapshot()
                            .language_settings_at(start, cx);
                        let text = if settings.hard_tabs {
                            let tab_size = settings.tab_size.get();
                            let insert_column =
                                start.to_display_point(&display_map).column();
                            let target_column = insert_column + padding;
                            let tab_column = insert_column - (insert_column % tab_size);
                            let tabs = (target_column - tab_column) / tab_size;
                            let spaces = if tabs > 0 {
                                target_column - (tab_column + tabs * tab_size)
                            } else {
                                target_column - insert_column
                            };
                            "\t".repeat(tabs as usize) + &" ".repeat(spaces as usize)
                        } else {
                            " ".repeat(padding as usize)
                        };
                        edits.push((start..start, text));
                    }
                    if !edits.is_empty() {
                        editor.edit(edits, cx);
                    }
                }
            });
        });
    }

    /// Kakoune's `alt-z`/`alt-Z` combine menus: the saved selections are the
    /// receiver and are combined pairwise with the current ones (except for
    /// append). With `save`, the result replaces the saved slot instead of
    /// the editor's selections.
    fn kakoune_combine_selections(
        &mut self,
        kind: CombineKind,
        save: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.update_editor(cx, |vim, editor, cx| {
            if vim.kakoune_saved_selections.is_empty() {
                return;
            }
            let display_map = editor.display_snapshot(cx);
            let buffer_snapshot = display_map.buffer_snapshot();

            // `(range, head)` pairs; the head is needed for the leftmost and
            // rightmost cursor comparisons.
            let resolve =
                |start: MultiBufferOffset, end: MultiBufferOffset, reversed: bool| {
                    let head = if reversed { start } else { end };
                    (start..end, head)
                };
            let saved: Vec<_> = vim
                .kakoune_saved_selections
                .iter()
                .map(|selection| {
                    resolve(
                        selection.start.to_offset(buffer_snapshot),
                        selection.end.to_offset(buffer_snapshot),
                        selection.reversed,
                    )
                })
                .collect();
            let current: Vec<_> = editor
                .selections
                .all::<MultiBufferOffset>(&display_map)
                .into_iter()
                .map(|selection| resolve(selection.start, selection.end, selection.reversed))
                .collect();

            let combined: Vec<Range<MultiBufferOffset>> = if kind == CombineKind::Append {
                saved
                    .iter()
                    .chain(current.iter())
                    .map(|(range, _)| range.clone())
                    .collect()
            } else {
                // Kakoune errors when the counts differ.
                if saved.len() != current.len() {
                    return;
                }
                saved
                    .iter()
                    .zip(current.iter())
                    .map(|((saved_range, saved_head), (current_range, current_head))| {
                        match kind {
                            CombineKind::Union => {
                                saved_range.start.min(current_range.start)
                                    ..saved_range.end.max(current_range.end)
                            }
                            CombineKind::Intersect => {
                                let start = saved_range.start.max(current_range.start);
                                let end = saved_range.end.min(current_range.end);
                                if start <= end { start..end } else { end..start }
                            }
                            CombineKind::SelectLeftmost => {
                                if saved_head <= current_head {
                                    saved_range.clone()
                                } else {
                                    current_range.clone()
                                }
                            }
                            CombineKind::SelectRightmost => {
                                if saved_head >= current_head {
                                    saved_range.clone()
                                } else {
                                    current_range.clone()
                                }
                            }
                            CombineKind::SelectLongest => {
                                if saved_range.end.0 - saved_range.start.0
                                    >= current_range.end.0 - current_range.start.0
                                {
                                    saved_range.clone()
                                } else {
                                    current_range.clone()
                                }
                            }
                            CombineKind::SelectShortest => {
                                if saved_range.end.0 - saved_range.start.0
                                    <= current_range.end.0 - current_range.start.0
                                {
                                    saved_range.clone()
                                } else {
                                    current_range.clone()
                                }
                            }
                            CombineKind::Append => unreachable!(),
                        }
                    })
                    .collect()
            };

            if save {
                vim.kakoune_saved_selections = combined
                    .into_iter()
                    .enumerate()
                    .map(|(id, range)| Selection {
                        id,
                        start: buffer_snapshot.anchor_before(range.start),
                        end: buffer_snapshot.anchor_before(range.end),
                        reversed: false,
                        goal: SelectionGoal::None,
                    })
                    .collect();
            } else {
                editor.change_selections(Default::default(), window, cx, |s| {
                    s.select_ranges(combined);
                });
            }
        });
    }

    /// Records selection changes for Kakoune's selection history (`alt-u`).
    /// Called for every local selection change on the editor.
    pub(crate) fn kakoune_record_selection_history(&mut self, cx: &mut Context<Self>) {
        if self.mode != Mode::KakouneNormal || self.kakoune_restoring_selections {
            return;
        }
        let Some(editor) = self.editor() else {
            return;
        };
        let current = editor.read(cx).selections.disjoint_anchors().to_vec();
        if current == self.kakoune_last_selections {
            return;
        }
        if !self.kakoune_last_selections.is_empty() {
            self.kakoune_selection_undo
                .push(std::mem::take(&mut self.kakoune_last_selections));
            if self.kakoune_selection_undo.len() > 100 {
                self.kakoune_selection_undo.remove(0);
            }
            self.kakoune_selection_redo.clear();
        }
        self.kakoune_last_selections = current;
    }

    /// Kakoune's `alt-u`/`alt-U`: walk the selection history.
    fn kakoune_selection_history_step(
        &mut self,
        redo: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entry = if redo {
            self.kakoune_selection_redo.pop()
        } else {
            self.kakoune_selection_undo.pop()
        };
        let Some(entry) = entry else {
            return;
        };
        let current = std::mem::replace(&mut self.kakoune_last_selections, entry.clone());
        if redo {
            self.kakoune_selection_undo.push(current);
        } else {
            self.kakoune_selection_redo.push(current);
        }
        self.kakoune_restoring_selections = true;
        self.update_editor(cx, |_, editor, cx| {
            editor.change_selections(Default::default(), window, cx, |s| {
                s.select_anchors(entry);
            });
        });
        self.kakoune_restoring_selections = false;
    }

    /// Kakoune's view menu horizontal commands: center the cursor, scroll by
    /// columns, or put the cursor on the window's leftmost/rightmost column.
    fn kakoune_scroll_view(
        &mut self,
        kind: ScrollViewKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = Vim::take_count(cx).unwrap_or(1) as f64;
        self.update_editor(cx, |_, editor, cx| {
            let display_map = editor.display_snapshot(cx);
            let cursor_column = editor
                .selections
                .newest_anchor()
                .head()
                .to_display_point(&display_map)
                .column() as f64;
            let mut position = editor.scroll_position(cx);
            position.x = match kind {
                ScrollViewKind::Left => position.x - count,
                ScrollViewKind::Right => position.x + count,
                ScrollViewKind::Center => {
                    let Some(visible_columns) = editor.visible_column_count() else {
                        return;
                    };
                    cursor_column - visible_columns / 2.
                }
                ScrollViewKind::Leftmost => cursor_column,
                ScrollViewKind::Rightmost => {
                    let Some(visible_columns) = editor.visible_column_count() else {
                        return;
                    };
                    cursor_column - visible_columns + 1.
                }
            }
            .max(0.);
            editor.set_scroll_position(position, window, cx);
        });
    }

    /// Kakoune's `alt-J`: join the selected lines (or each cursor's line with
    /// the next), replacing each line break and the following indentation
    /// with a single space, and select the inserted spaces.
    fn kakoune_join_select_spaces(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_editor(cx, |_, editor, cx| {
            let display_map = editor.display_snapshot(cx);
            let buffer_snapshot = display_map.buffer_snapshot();
            let max_row = buffer_snapshot.max_point().row;

            let mut joins: Vec<Range<MultiBufferOffset>> = Vec::new();
            for selection in editor.selections.all::<Point>(&display_map) {
                let min_row = selection.start.row;
                let max_row_in_selection = if selection.end.column == 0
                    && selection.end.row > selection.start.row
                {
                    selection.end.row - 1
                } else {
                    selection.end.row
                };
                // A single-line selection joins its line with the next one;
                // a multi-line selection joins the lines it covers.
                let end_row = if min_row == max_row_in_selection {
                    (max_row_in_selection + 1).min(max_row)
                } else {
                    max_row_in_selection
                };
                for row in min_row..end_row {
                    let newline = Point::new(row, buffer_snapshot.line_len(MultiBufferRow(row)));
                    let mut indent_len = 0;
                    for c in buffer_snapshot.chars_at(Point::new(row + 1, 0)) {
                        if c == ' ' || c == '\t' {
                            indent_len += 1;
                        } else {
                            break;
                        }
                    }
                    let range = buffer_snapshot.point_to_offset(newline)
                        ..buffer_snapshot.point_to_offset(Point::new(row + 1, indent_len));
                    if joins.last() != Some(&range) {
                        joins.push(range);
                    }
                }
            }
            if joins.is_empty() {
                return;
            }

            let edits: Vec<_> = joins
                .iter()
                .map(|range| (range.clone(), " ".to_string()))
                .collect();
            // Select each inserted space, adjusting for the length changes of
            // the preceding replacements.
            let mut new_ranges = Vec::new();
            let mut removed = 0;
            for range in &joins {
                let start = MultiBufferOffset(range.start.0 - removed);
                new_ranges.push(start..MultiBufferOffset(start.0 + 1));
                removed += (range.end.0 - range.start.0) - 1;
            }

            editor.transact(window, cx, |editor, window, cx| {
                editor.edit(edits, cx);
                editor.change_selections(Default::default(), window, cx, |s| {
                    s.select_ranges(new_ranges);
                });
            });
        });
    }

    /// Collects the rows covered by the selections, deduplicated, accounting
    /// for full-line selections whose exclusive end sits on the next row.
    fn kakoune_selected_rows(&self, editor: &mut Editor, cx: &mut Context<Editor>) -> Vec<u32> {
        let display_map = editor.display_snapshot(cx);
        let mut rows = Vec::new();
        for selection in editor.selections.all::<Point>(&display_map) {
            let end_row = if selection.end.column == 0 && selection.end.row > selection.start.row {
                selection.end.row - 1
            } else {
                selection.end.row
            };
            rows.extend(selection.start.row..=end_row);
        }
        rows.sort_unstable();
        rows.dedup();
        rows
    }

    /// Kakoune's `alt->`: indent the selected lines, including empty ones
    /// (which plain `>` skips).
    fn kakoune_indent_including_empty(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let count = Vim::take_count(cx).unwrap_or(1);
        self.update_editor(cx, |vim, editor, cx| {
            let rows = vim.kakoune_selected_rows(editor, cx);
            let display_map = editor.display_snapshot(cx);
            let buffer_snapshot = display_map.buffer_snapshot();
            let mut edits = Vec::new();
            for row in rows {
                let start = Point::new(row, 0);
                let settings = buffer_snapshot.language_settings_at(start, cx);
                let indent = if settings.hard_tabs {
                    "\t".repeat(count)
                } else {
                    " ".repeat(settings.tab_size.get() as usize * count)
                };
                edits.push((start..start, indent));
            }
            if edits.is_empty() {
                return;
            }
            editor.transact(window, cx, |editor, _, cx| {
                editor.edit(edits, cx);
            });
        });
    }

    /// Kakoune's `alt-<`: unindent the selected lines, but leave incomplete
    /// indentation in place (which plain `<` removes).
    fn kakoune_outdent_keep_incomplete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let count = Vim::take_count(cx).unwrap_or(1) as u32;
        self.update_editor(cx, |vim, editor, cx| {
            let rows = vim.kakoune_selected_rows(editor, cx);
            let display_map = editor.display_snapshot(cx);
            let buffer_snapshot = display_map.buffer_snapshot();
            let mut edits = Vec::new();
            for row in rows {
                let line_start = Point::new(row, 0);
                let settings = buffer_snapshot.language_settings_at(line_start, cx);
                let tab_size = settings.tab_size.get();
                let target_width = tab_size * count;
                let mut width = 0;
                for (column, c) in buffer_snapshot.chars_at(line_start).enumerate() {
                    match c {
                        '\t' => width = (width / tab_size + 1) * tab_size,
                        ' ' => width += 1,
                        // The indentation ends before a full level: leave it.
                        _ => break,
                    }
                    if width >= target_width {
                        edits.push((line_start..Point::new(row, column as u32 + 1), String::new()));
                        break;
                    }
                }
            }
            if edits.is_empty() {
                return;
            }
            editor.transact(window, cx, |editor, _, cx| {
                editor.edit(edits, cx);
            });
        });
    }

    /// Kakoune's `alt-&`: copy the indentation of the main selection's first
    /// line (or the count-th selection's, one-based) to every selected line.
    fn kakoune_copy_indent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let count = Vim::take_count(cx);
        self.update_editor(cx, |_, editor, cx| {
            let display_map = editor.display_snapshot(cx);
            let buffer_snapshot = display_map.buffer_snapshot();
            let selections = editor.selections.all::<Point>(&display_map);

            let mut rows = Vec::new();
            for selection in &selections {
                let end_row = if selection.end.column == 0 && selection.end.row > selection.start.row
                {
                    selection.end.row - 1
                } else {
                    selection.end.row
                };
                rows.extend(selection.start.row..=end_row);
            }
            rows.sort_unstable();
            rows.dedup();

            let reference = match count {
                Some(count) if count >= 1 && count <= selections.len() => count - 1,
                Some(_) => return,
                None => {
                    let newest_id = editor.selections.newest_anchor().id;
                    selections
                        .iter()
                        .position(|selection| selection.id == newest_id)
                        .unwrap_or(0)
                }
            };
            let reference_row = selections[reference].start.row;

            let leading_blanks = |row: u32| {
                buffer_snapshot
                    .chars_at(Point::new(row, 0))
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect::<String>()
            };
            let indent = leading_blanks(reference_row);

            let mut edits = Vec::new();
            for &row in &rows {
                if row == reference_row {
                    continue;
                }
                let current = leading_blanks(row);
                if current != indent {
                    edits.push((
                        Point::new(row, 0)..Point::new(row, current.len() as u32),
                        indent.clone(),
                    ));
                }
            }
            if edits.is_empty() {
                return;
            }
            editor.transact(window, cx, |editor, _, cx| {
                editor.edit(edits, cx);
            });
        });
    }

    /// Kakoune's `alt-,`: drop the main (newest) selection, keeping the rest.
    fn kakoune_clear_main_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_editor(cx, |_, editor, cx| {
            let display_map = editor.display_snapshot(cx);
            let newest_id = editor.selections.newest_anchor().id;
            let selections = editor.selections.all::<MultiBufferOffset>(&display_map);
            if selections.len() <= 1 {
                return;
            }
            let new_ranges: Vec<_> = selections
                .iter()
                .filter(|selection| selection.id != newest_id)
                .map(|selection| selection.start..selection.end)
                .collect();
            editor.change_selections(Default::default(), window, cx, |s| {
                s.select_ranges(new_ranges);
            });
        });
    }

    /// Kakoune's `alt-A`/`alt-I`: replace each selection with every
    /// occurrence of the object it contains.
    pub(crate) fn kakoune_nested_object(
        &mut self,
        kind: NestedObjectKind,
        around: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.stop_recording(cx);
        self.update_editor(cx, |_, editor, cx| {
            let display_map = editor.display_snapshot(cx);
            let mut new_ranges = Vec::new();
            for selection in editor.selections.all::<MultiBufferOffset>(&display_map) {
                // Kakoune selections always cover at least one character.
                let end = if selection.start == selection.end {
                    movement::chars_after(&display_map, selection.end)
                        .next()
                        .map_or(selection.end, |(_, char_range)| char_range.end)
                } else {
                    selection.end
                };
                nested_ranges(
                    &display_map,
                    selection.start..end,
                    kind,
                    around,
                    &mut new_ranges,
                );
            }
            if new_ranges.is_empty() {
                return;
            }
            editor.change_selections(Default::default(), window, cx, |s| {
                s.select_ranges(new_ranges);
            });
        });
    }

    /// A punctuation character entered during an object selection acts as
    /// the delimiter: the object spans from the previous occurrence (or the
    /// one under the cursor) to the next one.
    pub(crate) fn kakoune_delimiter_object(
        &mut self,
        delimiter: char,
        around: bool,
        target: KakouneObjectTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.kakoune_scanned_object(
            target,
            move |map, cursor| {
                let open = if char_at(map, cursor) == Some(delimiter) {
                    cursor
                } else {
                    movement::chars_before(map, cursor)
                        .find(|(c, _)| *c == delimiter)
                        .map(|(_, range)| range.start)?
                };
                let open_end = next_char_start(map, open)?;
                let (_, close) = movement::chars_after(map, open_end)
                    .find(|(c, _)| *c == delimiter)?;
                if around {
                    Some(open..close.end)
                } else {
                    Some(open_end..close.start)
                }
            },
            window,
            cx,
        );
    }

    /// Applies an object selection whose range comes from a character scan
    /// around the cursor (the number and whitespace objects), honoring the
    /// pending object target like `normal_object` does for vim objects.
    fn kakoune_scanned_object(
        &mut self,
        target: KakouneObjectTarget,
        range_for: impl Fn(&DisplaySnapshot, MultiBufferOffset) -> Option<Range<MultiBufferOffset>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.stop_recording(cx);
        self.update_editor(cx, |_, editor, cx| {
            editor.change_selections(Default::default(), window, cx, |s| {
                s.move_with(&mut |map, selection| {
                    let cursor = if selection.reversed || selection.is_empty() {
                        selection.head()
                    } else {
                        movement::left(map, selection.head())
                    };
                    let Some(range) = range_for(map, cursor.to_offset(map, Bias::Left)) else {
                        return;
                    };
                    let start = range.start.to_display_point(map);
                    let end = range.end.to_display_point(map);
                    match target {
                        KakouneObjectTarget::Whole => {
                            selection.set_head_tail(end, start, SelectionGoal::None);
                        }
                        KakouneObjectTarget::ToStart { extend } => {
                            if extend {
                                selection.set_head(start, SelectionGoal::None);
                            } else {
                                selection.set_head_tail(
                                    start,
                                    movement::right(map, cursor),
                                    SelectionGoal::None,
                                );
                            }
                        }
                        KakouneObjectTarget::ToEnd { extend } => {
                            if extend {
                                selection.set_head(end, SelectionGoal::None);
                            } else {
                                selection.set_head_tail(end, cursor, SelectionGoal::None);
                            }
                        }
                    }
                });
            });
        });
    }

    /// Kakoune's `alt-o`/`alt-O`: add empty lines around the cursor's line
    /// while staying in normal mode and keeping the selections in place.
    fn kakoune_add_line(&mut self, above: bool, window: &mut Window, cx: &mut Context<Self>) {
        let count = Vim::take_count(cx).unwrap_or(1);
        self.update_editor(cx, |_, editor, cx| {
            let display_map = editor.display_map.update(cx, |map, cx| map.snapshot(cx));
            let selections = editor.selections.all::<Point>(&display_map);
            let buffer_snapshot = display_map.buffer_snapshot();

            let text = "\n".repeat(count);
            let mut edits = Vec::new();
            for selection in &selections {
                let row = selection.head().row;
                let position = if above {
                    Point::new(row, 0)
                } else {
                    Point::new(row, buffer_snapshot.line_len(MultiBufferRow(row)))
                };
                edits.push((position..position, text.clone()));
            }

            editor.transact(window, cx, |editor, _, cx| {
                editor.edit(edits, cx);
            });
        });
    }

    /// Kakoune's `x`: expand each selection to cover whole lines, including
    /// the trailing end-of-line.
    fn kakoune_expand_to_lines(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_editor(cx, |_, editor, cx| {
            let display_map = editor.display_map.update(cx, |map, cx| map.snapshot(cx));
            let mut selections = editor.selections.all::<Point>(&display_map);
            let max_point = display_map.buffer_snapshot().max_point();

            for selection in &mut selections {
                let last_row = if selection.end.column == 0 && selection.end.row > selection.start.row
                {
                    selection.end.row - 1
                } else {
                    selection.end.row
                };
                selection.start = Point::new(selection.start.row, 0);
                selection.end = Point::new(last_row + 1, 0).min(max_point);
                // Kakoune gives the cursor a `max_column` target so that
                // vertical movement keeps hugging line ends.
                selection.goal = SelectionGoal::HorizontalPosition(f64::INFINITY);
            }

            editor.change_selections(Default::default(), window, cx, |s| {
                s.select(selections);
            });
        });
    }

    /// Kakoune's `alt-x`: trim each selection to only cover whole lines,
    /// excluding the last end-of-line. Selections that don't span a full
    /// line are left untouched.
    fn kakoune_trim_to_lines(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_editor(cx, |_, editor, cx| {
            let display_map = editor.display_map.update(cx, |map, cx| map.snapshot(cx));
            let mut selections = editor.selections.all::<Point>(&display_map);
            let buffer_snapshot = display_map.buffer_snapshot();
            let max_point = buffer_snapshot.max_point();

            for selection in &mut selections {
                let mut start = selection.start;
                let mut end = selection.end;
                if start.column != 0 {
                    start = Point::new(start.row + 1, 0);
                }
                let end_is_line_boundary = end.column == 0
                    || (end == max_point
                        && end.column == buffer_snapshot.line_len(MultiBufferRow(end.row)));
                if !end_is_line_boundary {
                    if end.row == 0 {
                        continue;
                    }
                    end = Point::new(end.row, 0);
                }
                if start >= end {
                    continue;
                }
                selection.start = start;
                selection.end = end;
                selection.goal = SelectionGoal::HorizontalPosition(f64::INFINITY);
            }

            editor.change_selections(Default::default(), window, cx, |s| {
                s.select(selections);
            });
        });
    }
}

#[cfg(test)]
mod test {
    use crate::{
        state::{Mode, Operator},
        test::VimTestContext,
    };

    #[gpui::test]
    async fn test_initial_mode(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        cx.set_state("ˇhello", Mode::KakouneNormal);
        assert_eq!(cx.mode(), Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_insert_round_trip(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        cx.set_state("ˇhello", Mode::KakouneNormal);
        cx.simulate_keystrokes("i");
        assert_eq!(cx.mode(), Mode::Insert);
        cx.simulate_keystrokes("a b c");
        // Unlike vim, escaping does not shift the cursor to the left.
        cx.simulate_keystrokes("escape");
        cx.assert_state("abcˇhello", Mode::KakouneNormal);

        cx.simulate_keystrokes("a");
        assert_eq!(cx.mode(), Mode::Insert);
        cx.simulate_keystrokes("escape");
        assert_eq!(cx.mode(), Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_kakoune_wins_over_helix(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_helix();
        cx.enable_kakoune();

        cx.set_state("ˇhello", Mode::KakouneNormal);
        cx.simulate_keystrokes("i");
        cx.simulate_keystrokes("escape");
        assert_eq!(cx.mode(), Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_word_motions(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // `w` selects the word and following whitespace, cursor on the last
        // whitespace character.
        cx.set_state("ˇThe quick brown", Mode::KakouneNormal);
        cx.simulate_keystrokes("w");
        cx.assert_state("«The ˇ»quick brown", Mode::KakouneNormal);
        cx.simulate_keystrokes("w");
        cx.assert_state("The «quick ˇ»brown", Mode::KakouneNormal);

        // From the last character of a word, `w` selects only the whitespace.
        cx.set_state("Thˇe quick", Mode::KakouneNormal);
        cx.simulate_keystrokes("w");
        cx.assert_state("The« ˇ»quick", Mode::KakouneNormal);

        // `e` selects through to the end of the next word.
        cx.set_state("ˇThe quick brown", Mode::KakouneNormal);
        cx.simulate_keystrokes("e");
        cx.assert_state("«Theˇ» quick brown", Mode::KakouneNormal);
        cx.simulate_keystrokes("e");
        cx.assert_state("The« quickˇ» brown", Mode::KakouneNormal);

        // `b` selects back to the start of the word, including preceding
        // whitespace when the cursor sits on a word boundary.
        cx.set_state("The quick brˇown", Mode::KakouneNormal);
        cx.simulate_keystrokes("b");
        cx.assert_state("The quick «ˇbro»wn", Mode::KakouneNormal);
        cx.simulate_keystrokes("b");
        cx.assert_state("The «ˇquick »brown", Mode::KakouneNormal);

        // A count repeats the motion.
        cx.set_state("ˇone two three four", Mode::KakouneNormal);
        cx.simulate_keystrokes("2 w");
        cx.assert_state("one «two ˇ»three four", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_extend_word_motions(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        cx.set_state("«Theˇ» quick brown", Mode::KakouneNormal);
        cx.simulate_keystrokes("shift-w");
        cx.assert_state("«The ˇ»quick brown", Mode::KakouneNormal);
        cx.simulate_keystrokes("shift-w");
        cx.assert_state("«The quick ˇ»brown", Mode::KakouneNormal);

        // Extending keeps the anchor in place while the cursor moves to where
        // `b` would put it.
        cx.set_state("The quick br«oˇ»wn", Mode::KakouneNormal);
        cx.simulate_keystrokes("shift-b");
        cx.assert_state("The quick «ˇbro»wn", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_extend_char_motions(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        cx.set_state("ˇabcd", Mode::KakouneNormal);
        cx.simulate_keystrokes("shift-l shift-l");
        cx.assert_state("«abcˇ»d", Mode::KakouneNormal);
        cx.simulate_keystrokes("shift-h");
        cx.assert_state("«abˇ»cd", Mode::KakouneNormal);

        cx.set_state(
            indoc::indoc! {"
            one
            twˇo
            three"},
            Mode::KakouneNormal,
        );
        cx.simulate_keystrokes("shift-j");
        cx.assert_state(
            indoc::indoc! {"
            one
            tw«o
            thrˇ»ee"},
            Mode::KakouneNormal,
        );
    }

    #[gpui::test]
    async fn test_find_motions(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // `f` selects up to and including the next occurrence.
        cx.set_state("ˇone two three", Mode::KakouneNormal);
        cx.simulate_keystrokes("f t");
        cx.assert_state("«one tˇ»wo three", Mode::KakouneNormal);

        // `t` selects until (excluding) the next occurrence.
        cx.set_state("ˇone two three", Mode::KakouneNormal);
        cx.simulate_keystrokes("t t");
        cx.assert_state("«one ˇ»two three", Mode::KakouneNormal);

        // `F` extends up to and including the next occurrence.
        cx.set_state("«one tˇ»wo three", Mode::KakouneNormal);
        cx.simulate_keystrokes("shift-f r");
        cx.assert_state("«one two thrˇ»ee", Mode::KakouneNormal);

        // `alt-f` selects backwards to and including the previous occurrence.
        cx.set_state("one two thrˇee", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-f o");
        cx.assert_state("one tw«ˇo thre»e", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_select_to_line_bounds(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        cx.set_state("one ˇtwo three", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-l");
        cx.assert_state("one «two threeˇ»", Mode::KakouneNormal);

        cx.set_state("one ˇtwo three", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-h");
        cx.assert_state("«ˇone t»wo three", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_expand_and_trim_lines(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // `x` expands to the full line, including the end-of-line, and is
        // idempotent.
        cx.set_state(
            indoc::indoc! {"
            one
            tˇwo
            three"},
            Mode::KakouneNormal,
        );
        cx.simulate_keystrokes("x");
        cx.assert_state(
            indoc::indoc! {"
            one
            «two
            ˇ»three"},
            Mode::KakouneNormal,
        );
        cx.simulate_keystrokes("x");
        cx.assert_state(
            indoc::indoc! {"
            one
            «two
            ˇ»three"},
            Mode::KakouneNormal,
        );

        // `alt-x` trims partially selected lines.
        cx.set_state(
            indoc::indoc! {"
            o«ne
            two
            thˇ»ree"},
            Mode::KakouneNormal,
        );
        cx.simulate_keystrokes("alt-x");
        cx.assert_state(
            indoc::indoc! {"
            one
            «two
            ˇ»three"},
            Mode::KakouneNormal,
        );
    }

    #[gpui::test]
    async fn test_line_selection_vertical_movement(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // `x` gives the cursor an end-of-line target column, so `J` extends
        // over the whole line below regardless of the original column.
        cx.set_state(
            indoc::indoc! {"
            oˇne
            twolonger
            three"},
            Mode::KakouneNormal,
        );
        cx.simulate_keystrokes("x shift-j");
        cx.assert_state(
            indoc::indoc! {"
            «one
            twolonger
            ˇ»three"},
            Mode::KakouneNormal,
        );

        // Plain `j` collapses to the end of the line below, like kakoune.
        cx.set_state(
            indoc::indoc! {"
            oˇne
            twolonger
            three"},
            Mode::KakouneNormal,
        );
        cx.simulate_keystrokes("x j");
        cx.assert_state(
            indoc::indoc! {"
            one
            twolongerˇ
            three"},
            Mode::KakouneNormal,
        );
    }

    #[gpui::test]
    async fn test_goto(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        cx.set_state(
            indoc::indoc! {"
            one
            two
            thrˇee"},
            Mode::KakouneNormal,
        );
        cx.simulate_keystrokes("g g");
        cx.assert_state(
            indoc::indoc! {"
            ˇone
            two
            three"},
            Mode::KakouneNormal,
        );

        cx.simulate_keystrokes("g e");
        cx.assert_state(
            indoc::indoc! {"
            one
            two
            threˇe"},
            Mode::KakouneNormal,
        );

        cx.simulate_keystrokes("g h");
        cx.assert_state(
            indoc::indoc! {"
            one
            two
            ˇthree"},
            Mode::KakouneNormal,
        );

        // `G` extends the selection to the goto target.
        cx.simulate_keystrokes("shift-g l");
        cx.assert_state(
            indoc::indoc! {"
            one
            two
            «threeˇ»"},
            Mode::KakouneNormal,
        );
    }

    #[gpui::test]
    async fn test_search(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // `/` selects the next match.
        cx.set_state("ˇone two three two", Mode::KakouneNormal);
        cx.simulate_keystrokes("/ t w o");
        cx.simulate_keystrokes("enter");
        cx.assert_state("one «twoˇ» three two", Mode::KakouneNormal);

        // `n` selects the next match, `N` adds a selection with it.
        cx.simulate_keystrokes("n");
        cx.assert_state("one two three «twoˇ»", Mode::KakouneNormal);

        // `?` extends the selection up to the end of the next match.
        cx.set_state("ˇone two three two", Mode::KakouneNormal);
        cx.simulate_keystrokes("? t w o");
        cx.simulate_keystrokes("enter");
        cx.assert_state("«one twoˇ» three two", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_changes(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // `d` deletes the selection (yanking it).
        cx.set_state("one «twoˇ» three", Mode::KakouneNormal);
        cx.simulate_keystrokes("d");
        cx.assert_state("one ˇ three", Mode::KakouneNormal);

        // `y` yanks and `p` pastes after the selection end, selecting the
        // pasted text.
        cx.set_state("«oneˇ» two", Mode::KakouneNormal);
        cx.simulate_keystrokes("y p");
        cx.assert_state("one«oneˇ» two", Mode::KakouneNormal);

        // `c` deletes the selection and enters insert mode.
        cx.set_state("one «twoˇ» three", Mode::KakouneNormal);
        cx.simulate_keystrokes("c");
        assert_eq!(cx.mode(), Mode::Insert);
        cx.simulate_keystrokes("x escape");
        cx.assert_state("one xˇ three", Mode::KakouneNormal);

        // `r` replaces every character of the selection.
        cx.set_state("one «twoˇ» three", Mode::KakouneNormal);
        cx.simulate_keystrokes("r z");
        cx.assert_state("one «zzzˇ» three", Mode::KakouneNormal);

        // Case conversions.
        cx.set_state("«oneˇ» two", Mode::KakouneNormal);
        cx.simulate_keystrokes("~");
        cx.assert_state("«ONEˇ» two", Mode::KakouneNormal);
        cx.simulate_keystrokes("`");
        cx.assert_state("«oneˇ» two", Mode::KakouneNormal);

        // Undo and redo.
        cx.simulate_keystrokes("u");
        cx.assert_state("«ONEˇ» two", Mode::KakouneNormal);
        cx.simulate_keystrokes("shift-u");
        cx.assert_state("«oneˇ» two", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_add_lines_around_cursor(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        cx.set_state(
            indoc::indoc! {"
            one
            tˇwo
            three"},
            Mode::KakouneNormal,
        );
        cx.simulate_keystrokes("alt-o");
        cx.assert_state(
            indoc::indoc! {"
            one
            tˇwo

            three"},
            Mode::KakouneNormal,
        );
        cx.simulate_keystrokes("alt-shift-o");
        cx.assert_state(
            indoc::indoc! {"
            one

            tˇwo

            three"},
            Mode::KakouneNormal,
        );
    }

    #[gpui::test]
    async fn test_selection_duplication(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // `C` duplicates selections on the following line, `,` keeps only the
        // newest selection.
        cx.set_state(
            indoc::indoc! {"
            oˇne
            two"},
            Mode::KakouneNormal,
        );
        cx.simulate_keystrokes("shift-c");
        cx.assert_state(
            indoc::indoc! {"
            oˇne
            tˇwo"},
            Mode::KakouneNormal,
        );
        cx.simulate_keystrokes(",");
        cx.assert_state(
            indoc::indoc! {"
            one
            tˇwo"},
            Mode::KakouneNormal,
        );
    }

    #[gpui::test]
    async fn test_nested_object_selection(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // `alt-I w` selects every word within the selection; `alt-A w` also
        // takes the trailing blanks.
        cx.set_state("«one, two threeˇ» rest", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-shift-i w");
        cx.assert_state("«oneˇ», «twoˇ» «threeˇ» rest", Mode::KakouneNormal);

        cx.set_state("«one, two threeˇ» rest", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-shift-a w");
        cx.assert_state("«oneˇ», «two ˇ»«threeˇ» rest", Mode::KakouneNormal);

        // Brackets select the top-level balanced regions; a count picks a
        // nesting depth.
        cx.set_state("«(a) x (b(c))ˇ» end", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-shift-a b");
        cx.assert_state("«(a)ˇ» x «(b(c))ˇ» end", Mode::KakouneNormal);

        cx.set_state("«(a) x (b(c))ˇ» end", Mode::KakouneNormal);
        cx.simulate_keystrokes("2 alt-shift-i b");
        cx.assert_state("(a) x (b(«cˇ»)) end", Mode::KakouneNormal);

        // Quotes alternate between opening and closing a region.
        cx.set_state("«'a' x 'b'ˇ» end", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-shift-i q");
        cx.assert_state("'«aˇ»' x '«bˇ»' end", Mode::KakouneNormal);

        // Numbers within the selection.
        cx.set_state("«a 12, b -3.4ˇ» 5", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-shift-a n");
        cx.assert_state("a «12ˇ», b «-3.4ˇ» 5", Mode::KakouneNormal);

        // Punctuation delimiters work nested too.
        cx.set_state("«a/b/c/dˇ» end", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-shift-i /");
        cx.assert_state("a/«bˇ»/c/«dˇ» end", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_punctuation_delimiter_object(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // The doc's example: on the `o` of `/home/bar`, `alt-a /` selects
        // `/home/`.
        cx.set_state("/hˇome/bar", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-a /");
        cx.assert_state("«/home/ˇ»bar", Mode::KakouneNormal);

        cx.set_state("/hˇome/bar", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-i /");
        cx.assert_state("/«homeˇ»/bar", Mode::KakouneNormal);

        // Works with any punctuation and with the bound targets.
        cx.set_state("a, ˇb, c", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-i ,");
        cx.assert_state("a,« bˇ», c", Mode::KakouneNormal);

        // Without a matching pair, nothing happens.
        cx.set_state("no delimˇiters here", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-a /");
        cx.assert_state("no delimˇiters here", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_object_selection(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // `alt-i` selects the inner object, `alt-a` the whole object.
        cx.set_state("foo(bˇar)baz", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-i b");
        cx.assert_state("foo(«barˇ»)baz", Mode::KakouneNormal);

        cx.set_state("foo(bˇar)baz", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-a b");
        cx.assert_state("foo«(bar)ˇ»baz", Mode::KakouneNormal);

        cx.set_state("say 'hˇello' now", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-i q");
        cx.assert_state("say '«helloˇ»' now", Mode::KakouneNormal);

        // `[`/`]` select from the cursor to the object's start/end.
        cx.set_state("foo(bar bˇaz qux)end", Mode::KakouneNormal);
        // The cursor's character stays selected.
        cx.simulate_keystrokes("[ b");
        cx.assert_state("foo«ˇ(bar ba»z qux)end", Mode::KakouneNormal);

        cx.set_state("foo(bar bˇaz qux)end", Mode::KakouneNormal);
        cx.simulate_keystrokes("] b");
        cx.assert_state("foo(bar b«az qux)ˇ»end", Mode::KakouneNormal);

        // `{`/`}` extend the selection to the object's start/end.
        cx.set_state("foo(bar «bazˇ» qux)end", Mode::KakouneNormal);
        cx.simulate_keystrokes("} b");
        cx.assert_state("foo(bar «baz qux)ˇ»end", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_split_selections(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        let status_label = |cx: &mut VimTestContext| {
            cx.update_editor(|editor, _, cx| {
                editor
                    .addon::<crate::VimAddon>()
                    .unwrap()
                    .entity
                    .read(cx)
                    .status_label
                    .clone()
            })
        };

        // `S` splits the selection on the regex matches, showing the pending
        // transformation in the mode indicator while the prompt is open.
        cx.set_state("«one, two, threeˇ» end", Mode::KakouneNormal);
        cx.simulate_keystrokes("shift-s , space");
        assert_eq!(status_label(&mut cx).as_deref(), Some("split:"));
        cx.simulate_keystrokes("enter");
        assert_eq!(status_label(&mut cx), None);
        cx.assert_state("«oneˇ», «twoˇ», «threeˇ» end", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_keep_and_clear_matching(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // Three selections, one per line; keep the ones matching "o".
        cx.set_state(
            indoc::indoc! {"
            «oneˇ»
            «twoˇ»
            «threeˇ»"},
            Mode::KakouneNormal,
        );
        cx.simulate_keystrokes("alt-k o");
        cx.simulate_keystrokes("enter");
        cx.assert_state(
            indoc::indoc! {"
            «oneˇ»
            «twoˇ»
            three"},
            Mode::KakouneNormal,
        );

        // Now clear the ones matching "n".
        cx.simulate_keystrokes("alt-shift-k n");
        cx.simulate_keystrokes("enter");
        cx.assert_state(
            indoc::indoc! {"
            one
            «twoˇ»
            three"},
            Mode::KakouneNormal,
        );
    }

    #[gpui::test]
    async fn test_boundary_chars_and_clear_main(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // `alt-S` selects the first and last characters of the selection.
        cx.set_state("«helloˇ» world", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-shift-s");
        cx.assert_state("«hˇ»ell«oˇ» world", Mode::KakouneNormal);

        // `alt-,` clears the main (newest) selection.
        cx.set_state(
            indoc::indoc! {"
            «oneˇ»
            «twoˇ»"},
            Mode::KakouneNormal,
        );
        cx.simulate_keystrokes("alt-,");
        cx.assert_state(
            indoc::indoc! {"
            «oneˇ»
            two"},
            Mode::KakouneNormal,
        );
    }

    #[gpui::test]
    async fn test_matching_pairs(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // From inside, `m` selects the next enclosed sequence: the scan finds
        // the closing bracket first and selects back to its opener.
        cx.set_state("foo(bar bˇaz qux)end", Mode::KakouneNormal);
        cx.simulate_keystrokes("m");
        cx.assert_state("foo«ˇ(bar baz qux)»end", Mode::KakouneNormal);

        // On an opening bracket, `m` selects to its match.
        cx.set_state("fooˇ(bar (baz) qux)end", Mode::KakouneNormal);
        cx.simulate_keystrokes("m");
        cx.assert_state("foo«(bar (baz) qux)ˇ»end", Mode::KakouneNormal);

        // Before any bracket, `m` scans forward to the next pair character.
        cx.set_state("ˇfoo (bar)", Mode::KakouneNormal);
        cx.simulate_keystrokes("m");
        cx.assert_state("foo «(bar)ˇ»", Mode::KakouneNormal);

        // `alt-m` scans backwards.
        cx.set_state("(foo) bˇar", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-m");
        cx.assert_state("«ˇ(foo)» bar", Mode::KakouneNormal);

        // `M` extends to the matching target.
        cx.set_state("«fooˇ»(bar)end", Mode::KakouneNormal);
        cx.simulate_keystrokes("shift-m");
        cx.assert_state("«foo(bar)ˇ»end", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_count_goto_line(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        cx.set_state(
            indoc::indoc! {"
            one
            two
            thˇree
            four"},
            Mode::KakouneNormal,
        );
        cx.simulate_keystrokes("2 g");
        cx.assert_state(
            indoc::indoc! {"
            one
            ˇtwo
            three
            four"},
            Mode::KakouneNormal,
        );

        cx.simulate_keystrokes("4 shift-g");
        cx.assert_state(
            indoc::indoc! {"
            one
            «two
            three
            fˇ»our"},
            Mode::KakouneNormal,
        );
    }

    #[gpui::test]
    async fn test_trim_and_merge(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // `_` unselects surrounding whitespace.
        cx.set_state("a« one ˇ»b", Mode::KakouneNormal);
        cx.simulate_keystrokes("_");
        cx.assert_state("a «oneˇ» b", Mode::KakouneNormal);

        // `alt-_` merges contiguous selections.
        cx.set_state("«oneˇ»« twoˇ» three", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-_");
        cx.assert_state("«one twoˇ» three", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_rotate_and_clear_main(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // Three selections; the newest (main) is the last one set. Rotating
        // forward wraps to the first, so clearing main then drops it.
        cx.set_state("«aˇ»1 «bˇ»2 «cˇ»3", Mode::KakouneNormal);
        cx.simulate_keystrokes(")");
        cx.simulate_keystrokes("alt-,");
        cx.assert_state("a1 «bˇ»2 «cˇ»3", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_rotate_content(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // Contents rotate forward; differing lengths exercise the offset
        // adjustments.
        cx.set_state("«oneˇ» «twoˇ» «threeˇ»", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-)");
        cx.assert_state("«threeˇ» «oneˇ» «twoˇ»", Mode::KakouneNormal);

        // Rotating backward undoes it.
        cx.simulate_keystrokes("alt-(");
        cx.assert_state("«oneˇ» «twoˇ» «threeˇ»", Mode::KakouneNormal);

        // A count rotates within groups of that size.
        cx.set_state("«aˇ»1 «bˇ»2 «cˇ»3 «dˇ»4", Mode::KakouneNormal);
        cx.simulate_keystrokes("2 alt-)");
        cx.assert_state("«bˇ»1 «aˇ»2 «dˇ»3 «cˇ»4", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_rotate_content_main_follows(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // The main (newest) selection is the last one set; after rotating
        // forward its content wraps to the first selection, so clearing the
        // main selection drops the first one.
        cx.set_state("«aˇ»1 «bˇ»2 «cˇ»3", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-)");
        cx.assert_state("«cˇ»1 «aˇ»2 «bˇ»3", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-,");
        cx.assert_state("c1 «aˇ»2 «bˇ»3", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_paste_all(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // Yank two selections, collapse to a cursor, then paste all pieces
        // after the cursor's character, selecting each piece.
        cx.set_state("«oneˇ» «twoˇ» rest", Mode::KakouneNormal);
        cx.simulate_keystrokes("y");
        cx.set_state("one two ˇrest", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-p");
        cx.assert_state("one two r«oneˇ»«twoˇ»est", Mode::KakouneNormal);

        // `alt-P` pastes all pieces before the selection.
        cx.set_state("one two ˇrest", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-shift-p");
        cx.assert_state("one two «oneˇ»«twoˇ»rest", Mode::KakouneNormal);

        // `alt-R` replaces each selection with all pieces.
        cx.set_state("one two «restˇ»", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-shift-r");
        cx.assert_state("one two «oneˇ»«twoˇ»", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_paste_all_linewise(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // Yank two full lines; pasting all inserts both below the cursor's
        // line.
        cx.set_state(
            indoc::indoc! {"
            oˇne
            two
            three"},
            Mode::KakouneNormal,
        );
        cx.simulate_keystrokes("x shift-j x y");
        cx.assert_state(
            indoc::indoc! {"
            «one
            two
            ˇ»three"},
            Mode::KakouneNormal,
        );
        cx.set_state(
            indoc::indoc! {"
            one
            two
            thˇree"},
            Mode::KakouneNormal,
        );
        cx.simulate_keystrokes("alt-p");
        cx.assert_state(
            indoc::indoc! {"
            one
            two
            three
            «one
            two
            ˇ»"},
            Mode::KakouneNormal,
        );
    }

    #[gpui::test]
    async fn test_lock_view_mode(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        cx.set_state("ˇone\ntwo\nthree", Mode::KakouneNormal);
        cx.simulate_keystrokes("shift-v");
        assert_eq!(cx.active_operator(), Some(Operator::KakouneView));

        // View keys keep the mode active so they can be repeated.
        cx.simulate_keystrokes("j j");
        assert_eq!(cx.active_operator(), Some(Operator::KakouneView));

        cx.simulate_keystrokes("escape");
        assert_eq!(cx.active_operator(), None);
    }

    #[gpui::test]
    async fn test_view_scroll_columns(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        let long_line = "word ".repeat(100);
        cx.set_state(&format!("ˇ{long_line}"), Mode::KakouneNormal);

        let scroll_x = |cx: &mut VimTestContext| {
            cx.update_editor(|editor, _, cx| editor.scroll_position(cx).x)
        };
        assert_eq!(scroll_x(&mut cx), 0.);

        cx.simulate_keystrokes("v l");
        assert_eq!(scroll_x(&mut cx), 1.);

        // The lock view mode repeats scrolling until escape.
        cx.simulate_keystrokes("shift-v l l l escape");
        assert_eq!(scroll_x(&mut cx), 4.);

        cx.simulate_keystrokes("v h");
        assert_eq!(scroll_x(&mut cx), 3.);

        // `v <` puts the cursor on the leftmost column.
        cx.simulate_keystrokes("v <");
        assert_eq!(scroll_x(&mut cx), 0.);
    }

    #[gpui::test]
    async fn test_combine_selections(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // Save «one», move to «two », then take the pairwise union.
        cx.set_state("«oneˇ» two three", Mode::KakouneNormal);
        cx.simulate_keystrokes("shift-z w w");
        cx.assert_state("one «two ˇ»three", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-z u");
        cx.assert_state("«one two ˇ»three", Mode::KakouneNormal);

        // Append keeps both selection sets.
        cx.set_state("«oneˇ» two three", Mode::KakouneNormal);
        cx.simulate_keystrokes("shift-z w w");
        cx.simulate_keystrokes("alt-z a");
        cx.assert_state("«oneˇ» «two ˇ»three", Mode::KakouneNormal);

        // The shortest of each pair wins with `-`.
        cx.set_state("«oneˇ» two three", Mode::KakouneNormal);
        cx.simulate_keystrokes("shift-z w w");
        cx.simulate_keystrokes("alt-z -");
        cx.assert_state("«oneˇ» two three", Mode::KakouneNormal);

        // `alt-Z` writes the combination into the saved slot instead; `z`
        // then restores it.
        cx.set_state("«oneˇ» two three", Mode::KakouneNormal);
        cx.simulate_keystrokes("shift-z w w");
        cx.simulate_keystrokes("alt-shift-z u");
        cx.assert_state("one «two ˇ»three", Mode::KakouneNormal);
        cx.simulate_keystrokes("z");
        cx.assert_state("«one two ˇ»three", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_selection_history(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        cx.set_state("ˇone two three", Mode::KakouneNormal);
        cx.simulate_keystrokes("w");
        cx.assert_state("«one ˇ»two three", Mode::KakouneNormal);
        cx.simulate_keystrokes("w");
        cx.assert_state("one «two ˇ»three", Mode::KakouneNormal);

        // `alt-u` walks the selection history backwards.
        cx.simulate_keystrokes("alt-u");
        cx.assert_state("«one ˇ»two three", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-u");
        cx.assert_state("ˇone two three", Mode::KakouneNormal);

        // `alt-U` walks it forwards again.
        cx.simulate_keystrokes("alt-shift-u");
        cx.assert_state("«one ˇ»two three", Mode::KakouneNormal);

        // A new selection change clears the redo history, so redoing after
        // it is a no-op.
        cx.simulate_keystrokes("; w");
        cx.assert_state("one «two ˇ»three", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-shift-u");
        cx.assert_state("one «two ˇ»three", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_insert_one_shot_normal(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // `alt-;` escapes to normal mode for a single command, then returns
        // to insert mode.
        cx.set_state("ˇone two", Mode::KakouneNormal);
        cx.simulate_keystrokes("i a b");
        cx.assert_state("abˇone two", Mode::Insert);
        cx.simulate_keystrokes("alt-;");
        assert_eq!(cx.mode(), Mode::KakouneNormal);
        // A single motion runs, then insert mode resumes.
        cx.simulate_keystrokes("l");
        assert_eq!(cx.mode(), Mode::Insert);
        cx.simulate_keystrokes("alt-;");
        assert_eq!(cx.mode(), Mode::KakouneNormal);
        // An operator likewise returns to insert after one command.
        cx.simulate_keystrokes("d");
        assert_eq!(cx.mode(), Mode::Insert);
    }

    #[gpui::test]
    async fn test_toggle_completions(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        cx.set_state("ˇone", Mode::KakouneNormal);
        cx.simulate_keystrokes("i");
        let enabled = |cx: &mut VimTestContext| {
            cx.update_editor(|editor, _, cx| editor.show_completions_on_input(cx))
        };
        let initial = enabled(&mut cx);
        cx.simulate_keystrokes("ctrl-o");
        assert_eq!(enabled(&mut cx), !initial);
        cx.simulate_keystrokes("ctrl-o");
        assert_eq!(enabled(&mut cx), initial);
    }

    #[gpui::test]
    async fn test_disable_hooks(cx: &mut gpui::TestAppContext) {
        use crate::state::KakouneHooksPhase;

        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        let hooks_state = |cx: &mut VimTestContext| {
            cx.update_editor(|editor, _, cx| {
                let vim = editor.addon::<crate::VimAddon>().unwrap().entity.read(cx);
                (vim.kakoune_hooks_disabled, vim.should_autoindent())
            })
        };

        // `\` arms the suppression for the next command only.
        cx.set_state("ˇone two", Mode::KakouneNormal);
        cx.simulate_keystrokes("\\");
        assert_eq!(
            hooks_state(&mut cx),
            (Some(KakouneHooksPhase::Armed), false)
        );
        cx.simulate_keystrokes("w");
        assert_eq!(
            hooks_state(&mut cx),
            (Some(KakouneHooksPhase::Active), false)
        );
        cx.simulate_keystrokes("w");
        assert_eq!(hooks_state(&mut cx), (None, true));

        // `\i` keeps hooks disabled for the whole insert session.
        cx.set_state("ˇone two", Mode::KakouneNormal);
        cx.simulate_keystrokes("\\ i");
        assert_eq!(
            hooks_state(&mut cx),
            (Some(KakouneHooksPhase::Active), false)
        );
        cx.simulate_keystrokes("x y z");
        assert_eq!(
            hooks_state(&mut cx),
            (Some(KakouneHooksPhase::Active), false)
        );
        cx.simulate_keystrokes("escape");
        cx.assert_state("xyzˇone two", Mode::KakouneNormal);
        assert_eq!(hooks_state(&mut cx), (None, true));
    }

    #[gpui::test]
    async fn test_save_restore_selections(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        cx.set_state("«oneˇ» two three", Mode::KakouneNormal);
        cx.simulate_keystrokes("shift-z");
        cx.simulate_keystrokes("w w");
        cx.assert_state("one «two ˇ»three", Mode::KakouneNormal);
        cx.simulate_keystrokes("z");
        cx.assert_state("«oneˇ» two three", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_align(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        cx.set_state(
            indoc::indoc! {"
            a «=ˇ» 1
            longer «=ˇ» 2"},
            Mode::KakouneNormal,
        );
        cx.simulate_keystrokes("&");
        cx.assert_state(
            indoc::indoc! {"
            a      «=ˇ» 1
            longer «=ˇ» 2"},
            Mode::KakouneNormal,
        );

        // Multiple selections per line form column groups, each aligned
        // independently.
        cx.set_state(
            indoc::indoc! {"
            a «=ˇ» 11 «#ˇ» x
            longer «=ˇ» 2 «#ˇ» y"},
            Mode::KakouneNormal,
        );
        cx.simulate_keystrokes("&");
        cx.assert_state(
            indoc::indoc! {"
            a      «=ˇ» 11 «#ˇ» x
            longer «=ˇ» 2  «#ˇ» y"},
            Mode::KakouneNormal,
        );

        // Multi-line selections are not aligned.
        cx.set_state(
            indoc::indoc! {"
            o«ne
            tˇ»wo
            thˇree"},
            Mode::KakouneNormal,
        );
        cx.simulate_keystrokes("&");
        cx.assert_state(
            indoc::indoc! {"
            o«ne
            tˇ»wo
            thˇree"},
            Mode::KakouneNormal,
        );
    }

    #[gpui::test]
    async fn test_align_with_hard_tabs(cx: &mut gpui::TestAppContext) {
        use gpui::UpdateGlobal;

        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();
        cx.cx.update(|_, cx| {
            settings::SettingsStore::update_global(cx, |store, cx| {
                store.update_user_settings(cx, |s| {
                    s.project.all_languages.defaults.hard_tabs = Some(true);
                    s.project.all_languages.defaults.tab_size = std::num::NonZeroU32::new(4);
                });
            })
        });

        // The first line's `=` sits at column 2 and must reach column 8: one
        // tab jumps from column 2 to the tabstop at 4, another to 8.
        cx.set_state("a «=ˇ» 1\nlonger! «=ˇ» 2", Mode::KakouneNormal);
        cx.simulate_keystrokes("&");
        cx.assert_state("a \t\t«=ˇ» 1\nlonger! «=ˇ» 2", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_number_object(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // Inner selects the digit run.
        cx.set_state("price 1ˇ23 end", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-i n");
        cx.assert_state("price «123ˇ» end", Mode::KakouneNormal);

        // Around also takes the dot and a leading minus.
        cx.set_state("pi -3.1ˇ4 x", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-a n");
        cx.assert_state("pi «-3.14ˇ» x", Mode::KakouneNormal);

        // Inner stops at the dot.
        cx.set_state("pi 3.1ˇ4 x", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-i n");
        cx.assert_state("pi 3.«14ˇ» x", Mode::KakouneNormal);

        // `]` selects from the cursor to the number's end.
        cx.set_state("a 12ˇ345 b", Mode::KakouneNormal);
        cx.simulate_keystrokes("] n");
        cx.assert_state("a 12«345ˇ» b", Mode::KakouneNormal);

        // Not on a number: no-op.
        cx.set_state("ˇword 42", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-i n");
        cx.assert_state("ˇword 42", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_whitespace_object(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // Inner selects the run of spaces and tabs.
        cx.set_state("one ˇ\t two", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-i space");
        cx.assert_state("one« \t ˇ»two", Mode::KakouneNormal);

        // Around also crosses newlines.
        cx.set_state(
            indoc::indoc! {"
            oneˇ \n two"},
            Mode::KakouneNormal,
        );
        cx.simulate_keystrokes("alt-a space");
        cx.assert_state(
            indoc::indoc! {"
            one« \n ˇ»two"},
            Mode::KakouneNormal,
        );

        // Inner stops at the newline.
        cx.set_state(
            indoc::indoc! {"
            oneˇ \n two"},
            Mode::KakouneNormal,
        );
        cx.simulate_keystrokes("alt-i space");
        cx.assert_state(
            indoc::indoc! {"
            one« ˇ»
             two"},
            Mode::KakouneNormal,
        );

        // Not on whitespace: no-op.
        cx.set_state("ˇone two", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-i space");
        cx.assert_state("ˇone two", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_join_select_spaces(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // A cursor joins its line with the next, selecting the space that
        // replaced the line break and the next line's indentation.
        cx.set_state("oˇne\n    two\nthree", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-shift-j");
        cx.assert_state("one« ˇ»two\nthree", Mode::KakouneNormal);

        // A multi-line selection joins all the lines it covers.
        cx.set_state("o«ne\ntwo\nthˇ»ree\nrest", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-shift-j");
        cx.assert_state("one« ˇ»two« ˇ»three\nrest", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_indent_variants(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // `alt->` indents empty lines too; plain strings because indoc would
        // strip the indentation under test.
        cx.set_state("o«ne\n\ntˇ»wo", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt->");
        cx.assert_state("    o«ne\n    \n    tˇ»wo", Mode::KakouneNormal);

        // `alt-<` removes one full indent level but leaves incomplete
        // indentation in place.
        cx.set_state("    o«ne\n  two\n        thˇ»ree", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-<");
        cx.assert_state("o«ne\n  two\n    thˇ»ree", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_copy_indent(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // The main selection is the newest (the last one in document order
        // after set_state); its indentation is copied to the other lines.
        // indoc would strip the indentation under test, so plain strings.
        cx.set_state("ˇone\n    ˇtwo\n  ˇthree", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-&");
        cx.assert_state("  ˇone\n  ˇtwo\n  ˇthree", Mode::KakouneNormal);

        // A count picks the reference selection (one-based).
        cx.set_state("ˇone\n    ˇtwo\n  ˇthree", Mode::KakouneNormal);
        cx.simulate_keystrokes("2 alt-&");
        cx.assert_state("    ˇone\n    ˇtwo\n    ˇthree", Mode::KakouneNormal);
    }

    #[gpui::test]
    async fn test_search_pattern(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        let query = |cx: &mut VimTestContext| {
            cx.workspace(|workspace, _, cx| {
                workspace
                    .active_pane()
                    .read(cx)
                    .toolbar()
                    .read(cx)
                    .item_of_type::<search::BufferSearchBar>()
                    .map(|bar| bar.read(cx).query(cx))
            })
        };

        // `*` sets the pattern from the selection, with word-boundary
        // assertions, and feeds `n` without moving the cursor.
        cx.set_state("«twoˇ» one two three two", Mode::KakouneNormal);
        cx.simulate_keystrokes("*");
        assert_eq!(query(&mut cx).as_deref(), Some("\\btwo\\b"));
        // The cursor did not move; focus stays in the document.
        cx.assert_state("«twoˇ» one two three two", Mode::KakouneNormal);
        cx.run_until_parked();
        cx.simulate_keystrokes("n");
        cx.assert_state("two one «twoˇ» three two", Mode::KakouneNormal);

        // Boundaries are detected per edge: a selection at the end of a word
        // gets a trailing `\b` but no leading one.
        cx.set_state("th«isˇ» island", Mode::KakouneNormal);
        cx.simulate_keystrokes("*");
        assert_eq!(query(&mut cx).as_deref(), Some("is\\b"));
        // A selection internal to a word on both edges gets no assertions.
        cx.set_state("is«laˇ»nd", Mode::KakouneNormal);
        cx.simulate_keystrokes("*");
        assert_eq!(query(&mut cx).as_deref(), Some("la"));

        // `alt-*` escapes regex metacharacters verbatim, without boundaries.
        cx.set_state("«a.bˇ» a.b axb", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-*");
        assert_eq!(query(&mut cx).as_deref(), Some("a\\.b"));
    }

    #[gpui::test]
    async fn test_selection_direction(cx: &mut gpui::TestAppContext) {
        let mut cx = VimTestContext::new(cx, true).await;
        cx.enable_kakoune();

        // `alt-;` flips the selection direction.
        cx.set_state("«abcˇ»def", Mode::KakouneNormal);
        cx.simulate_keystrokes("alt-;");
        cx.assert_state("«ˇabc»def", Mode::KakouneNormal);

        // `alt-:` ensures the selection faces forward.
        cx.simulate_keystrokes("alt-:");
        cx.assert_state("«abcˇ»def", Mode::KakouneNormal);

        // `;` collapses the selection to its cursor.
        cx.set_state("«abcˇ»def", Mode::KakouneNormal);
        cx.simulate_keystrokes(";");
        cx.assert_state("abˇcdef", Mode::KakouneNormal);
    }
}
