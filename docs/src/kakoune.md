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
- Selection manipulation: `;`, `alt-;`, `alt-:`, `x`, `alt-x`, `%`, `s`, `S`, `alt-s`, `alt-S`, `,`, `alt-,`, `alt-k`, `alt-K`, `C`, `alt-C`, `_`, `alt-_`, `(`, `)`, `&`, and content rotation with `alt-(`/`alt-)` (grouped by a count).
- Selection marks: `Z` saves the selections, `z` restores them.
- Object selection: `alt-i`/`alt-a` followed by an object key, and `[`/`]`/`{`/`}` to select or extend to an object's start or end.
- Changes: `i`, `a`, `I`, `A`, `o`, `O`, `alt-o`, `alt-O`, `d`, `alt-d`, `c`, `alt-c`, `y`, `p`, `P`, `r`, `R`, `alt-j`, `<`, `>`, `u`, `U`, `` ` ``, `~`, ``alt-` ``, and paste-all with `alt-p`/`alt-P`/`alt-R`.
- Search: `/` selects the next match, `?` extends to it, `n`/`N` select or add the next match (`alt-n`/`alt-N` for the previous one), and `*` sets the search pattern from the selection.
- Macros on `Q` (record) and `q` (replay), and registers via `"`.
- View commands: `v` followed by `v`/`c` (center), `t` (top), `b` (bottom), or `j`/`k` (scroll).

## Known deviations

- Registers, macros, and marks use Vim's machinery rather than Kakoune's register semantics; `Z`/`z` use a single slot instead of the `^` register.
- `&` aligns the selection cursors only; Kakoune's column-group alignment for multiple selections per line is not implemented (`alt-&` copy-indent is not implemented either).
- The number (`n`), whitespace (`space`), and custom (`c`) text objects are not implemented.
- Shell-driven commands (`|`, `!`, `$`, `@`), selection history (`alt-u`, `alt-U`), the lock view mode (`V`), the `alt-z` mark combinations, and user modes (`space`) are not implemented yet.
