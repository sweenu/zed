---
title: Kakoune Mode - Zed
description: Kakoune-style keybindings and modal editing in Zed. Selection-first editing built on top of Vim mode.
---

# Kakoune Mode

_Work in progress. Not all Kakoune keybindings are implemented yet._

Zed's Kakoune mode is an emulation layer that brings Kakoune-style keybindings and modal editing to Zed. It builds upon Zed's [Vim mode](./vim.md), so much of the core functionality is shared. Enabling `kakoune_mode` will also enable `vim_mode`.

For a guide on Vim-related features that are also available in Kakoune mode, please refer to our [Vim mode documentation](./vim.md).

For a detailed list of Kakoune's default keybindings, please visit the [official Kakoune documentation](https://github.com/mawww/kakoune/blob/master/doc/pages/keys.asciidoc).

## What's implemented

- Selection-first movement: lowercase movement keys (`h`, `j`, `k`, `l`, `w`, `b`, `e`, `f`, `t`, ...) replace the selection with the moved-over text, and their Shift-modified variants extend it instead.
- WORD variants on `alt-w`, `alt-b`, `alt-e` (extend with `alt-W`, `alt-B`, `alt-E`).
- Goto mode (`g` plus a key), where `G` extends the selection to the target, and count-prefixed `g`/`G` to go to a line.
- Matching pairs: `m`/`M` select or extend to the next enclosed sequence, `alt-m`/`alt-M` to the previous one.
- Selection manipulation: `;`, `alt-;`, `alt-:`, `x`, `alt-x`, `%`, `s`, `S`, `alt-s`, `alt-S`, `,`, `alt-,`, `alt-k`, `alt-K`, `C`, `alt-C`, `_`, `alt-_`, `(`, `)`, and content rotation with `alt-(`/`alt-)` (grouped by a count).
- Alignment: `&` aligns selections in per-line column groups, and `alt-&` copies the main selection's indentation to all selected lines.
- Selection marks: `Z` saves the selections, `z` restores them, and the `alt-z`/`alt-Z` menus combine the saved and current selections (append, union, intersection, leftmost/rightmost cursor, longest/shortest).
- Selection history: `alt-u` undoes the last selection change, `alt-U` redoes it.
- Object selection: `alt-i`/`alt-a` followed by an object key, and `[`/`]`/`{`/`}` to select or extend to an object's start or end.
- Changes: `i`, `a`, `I`, `A`, `o`, `O`, `alt-o`, `alt-O`, `d`, `alt-d`, `c`, `alt-c`, `y`, `p`, `P`, `r`, `R`, `alt-j`, `<`, `>`, `u`, `U`, `` ` ``, `~`, ``alt-` ``, and paste-all with `alt-p`/`alt-P`/`alt-R`.
- Search: `/` selects the next match, `?` extends to it, `n`/`N` select or add the next match (`alt-n`/`alt-N` for the previous one), and `*` sets the search pattern from the selection.
- Macros on `Q` (record) and `q` (replay), and registers via `"`.
- View commands: `v` followed by `v`/`c` (center), `t` (top), `b` (bottom), or `j`/`k` (scroll), and `V` to lock the view mode until escape.
- `\` disables hooks for the next command: autoindent is suppressed (including for a whole insert session, as in `\i`), and saving with `:w` or `ctrl-s` skips the formatter.

## Known deviations

Unimplemented keys (unless noted, they currently do nothing or fall back to
Zed's Vim binding for the same key):

- Shell-driven commands: `|`, `!`, `$`, and `@`/`alt-@` (tabs/spaces conversion).
- `+` and `alt-+` (duplicate selections, merge overlapping): Zed cannot represent overlapping selections. `+` currently falls back to Vim's next-line-start motion.
- `alt-J` (join lines and select the inserted spaces); plain `alt-j` works.
- `alt->` and `alt-<` (indent including empty lines, unindent keeping incomplete indents).
- `ctrl-j` and `ctrl-k` (move forward/backward in the changes history).
- `ctrl-s` saves the file instead of recording a jump point; `ctrl-o`/`ctrl-i` jump navigation itself works.
- `alt-*` (set the search pattern verbatim); `*` works.
- The custom (`c`) text object, the `alt-A`/`alt-I` nested object selections, and punctuation characters as object delimiters (such as `alt-a /`).
- In the view menus, `m` (center horizontally), `h`/`l` (scroll columns), and `<`/`>` are not bound.
- In insert mode, `alt-;` (one-shot normal command) and `ctrl-u` (commit an undo group) are not implemented; Zed's own insert and completion keys apply instead.

Behavioral differences:

- Registers, macros, and marks use Vim's machinery rather than Kakoune's register semantics; `Z`/`z` use a single slot instead of the `^` register.
- `.` uses Vim's repeat-last-change rather than Kakoune's repeat-last-insert, and `alt-.` repeats only `f`/`t` finds, not object selections.
- Instead of Kakoune's `aligntab` option, `&` pads with tabs when the buffer's language uses `hard_tabs`.
- Like Kakoune, `space` ships without default bindings; define your own user-mode style chords in your keymap with the `vim_mode == kakoune_normal` context.
- In the lock view mode, keys other than the view keys execute their normal binding and leave the mode, instead of being ignored.
- `\` covers autoindent and format-on-save; it does not suppress bracket auto-closing or completions.
- Keys that Kakoune leaves unbound may still do something in Zed, because Vim mode's base bindings apply wherever this mode doesn't override them.
