//! Kakoune mode.
//!
//! Like Helix mode (which was inspired by Kakoune), this is a selection-first
//! editing mode built on top of the vim infrastructure. Unlike Helix, Kakoune
//! has no select mode: selections are extended per-keystroke with
//! Shift-modified movement keys instead.

use editor::display_map::{DisplaySnapshot, ToDisplayPoint};
use editor::{Editor, MultiBufferOffset, movement};
use gpui::{Action, Context, Window, actions};
use language::{CharClassifier, CharKind};
use schemars::JsonSchema;
use serde::Deserialize;
use text::{Bias, SelectionGoal};

use crate::{Vim, motion::Motion};

actions!(
    vim,
    [
        /// Sets the direction of each selection to forward (cursor after anchor).
        KakouneEnsureForward,
    ]
);

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
    Matching,
    SelectToLineBegin,
    SelectToLineEnd,
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
            Self::Matching => Motion::Matching {
                match_quotes: false,
            },
        }
    }

    /// Kakoune motions that replace each selection with the range from the
    /// cursor to the target, instead of collapsing to the target.
    fn selects_to_target(self) -> bool {
        matches!(
            self,
            Self::SelectToLineBegin | Self::SelectToLineEnd | Self::Matching
        )
    }
}

pub fn register(editor: &mut Editor, cx: &mut Context<Vim>) {
    Vim::action(editor, cx, |vim, action: &KakouneMotion, window, cx| {
        let times = Vim::take_count(cx);
        Vim::take_forced_motion(cx);
        let motion = action.motion.to_motion(action.ignore_punctuation);
        if !action.extend && action.motion.selects_to_target() {
            let include_target = action.motion == KakouneMotionKind::Matching;
            vim.kakoune_select_to(motion, times, include_target, window, cx);
        } else {
            vim.kakoune_motion(motion, times, action.extend, window, cx);
        }
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
    /// the motion target (used by `alt-h`, `alt-l`, and `m`).
    ///
    /// `include_target` selects the character under the target as well; it
    /// only matters for forward targets, since a backward target is always
    /// covered by the selection start. `EndOfLine` must not include the
    /// target because its `move_point` already lands past the last character.
    fn kakoune_select_to(
        &mut self,
        motion: Motion,
        times: Option<usize>,
        include_target: bool,
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
                        let head = if include_target {
                            movement::right(map, target)
                        } else {
                            target
                        };
                        selection.set_head_tail(head, cursor, SelectionGoal::None);
                    }
                })
            });
        });
    }
}

#[cfg(test)]
mod test {
    use crate::{state::Mode, test::VimTestContext};

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
