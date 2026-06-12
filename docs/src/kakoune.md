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
- Goto mode (`g` plus a key), where `G` extends the selection to the target.
- Selection manipulation: `;`, `alt-;`, `alt-:`, `x`, `alt-x`, `%`, `s`, `S`, `alt-s`, `alt-S`, `,`, `alt-,`, `alt-k`, `alt-K`, `C`, `alt-C`.
- Object selection: `alt-i`/`alt-a` followed by an object key, and `[`/`]`/`{`/`}` to select or extend to an object's start or end.
- Changes: `i`, `a`, `I`, `A`, `o`, `O`, `alt-o`, `alt-O`, `d`, `alt-d`, `c`, `alt-c`, `y`, `p`, `P`, `r`, `R`, `alt-j`, `<`, `>`, `u`, `U`, `` ` ``, `~`, ``alt-` ``.
- Search: `/` selects the next match, `?` extends to it, `n`/`N` select or add the next match (`alt-n`/`alt-N` for the previous one).

## Known deviations

- A count followed by a bare `g` or `G` does not go to the given line; use `3 g g` instead of `3 g`.
- `m` approximates Kakoune's matching-pair selection with Vim's `%` matching: it selects from the cursor to the matching bracket rather than to the next enclosed sequence. `alt-m`/`alt-M` are not implemented.
- `*` uses Vim's behavior (it sets the search pattern from the word under the cursor and moves to the next match) instead of only setting the pattern from the selection.
- The number (`n`), whitespace (`space`), and custom (`c`) text objects are not implemented.
- Shell-driven commands (`|`, `!`, `$`, `&`, `@`), content rotation (`alt-(`, `alt-)`), marks (`Z`, `z`), view mode (`v`, `V`), user modes (`space`), and named registers are not implemented yet.
