# Fork briefing: sweenu/zed (Kakoune mode)

This is sweenu's fork of zed-industries/zed. Its only purpose is to maintain
**Kakoune mode** (a modal editing mode built on the vim crate) and to ship
installable builds that track Zed's **preview** releases. The branch layout is
unusual and force-pushes are routine — read this before touching anything.

This file is fork-only: upstream has `CLAUDE.md -> .rules` (tracked), so fork
instructions live here where daily rebases can never conflict with upstream.
It should exist on both `main` and `kakoune-mode`; if you change it on one
branch, cherry-pick the change to the other.

## Branch model

- **`main`** (default branch): a snapshot of upstream main plus the fork-only
  CI workflows and this file. It exists because GitHub only fires `schedule`
  triggers from the default branch. It is NOT kept in sync with upstream and
  does NOT contain the kakoune code. Only workflow/automation changes belong
  here.
- **`kakoune-mode`**: the real work branch. ~32 kakoune commits sitting
  directly on top of `upstream/main`, rebased (history rewritten!) daily by
  CI. All kakoune feature and fix work happens here. Because of the daily
  rebase: always start from a fresh fetch, never plain `git push --force`
  (use `--force-with-lease`), and expect local clones to be stale.
- **`preview-build`**: machine output only — the kakoune commits replayed
  onto the latest upstream preview tag. Force-pushed by CI on every release
  build. Never commit to it manually.

## Automation (all workflow files live on `main`)

- **`sync-upstream.yml`** (daily 06:00 UTC): rebases `kakoune-mode` onto
  `upstream/main`, gates with `cargo test -p vim && cargo build -p zed -p
  settings_ui`, and force-pushes only on success. On conflict or test failure
  it pushes nothing, opens/refreshes an issue labeled `upstream-sync`, and
  fails the run.
- **`build-fork-release.yml`** (daily 08:00 UTC): when upstream cuts a new
  preview tag (`vX.Y.Z-pre`), replays the kakoune commits onto that tag,
  builds Linux + macOS bundles as the `dev` release channel (so they coexist
  with an official Zed install), publishes a `kakoune-vX.Y.Z-pre` release on
  the fork, and pushes a nix build to a self-hosted attic cache. Failures
  open/refresh an issue labeled `fork-release-build`.
- **`sync-autofix.yml`**: fires when either workflow above fails. Runs Claude
  Code (authenticated with the `CLAUDE_CODE_OAUTH_TOKEN` secret from a
  Pro/Max subscription, not an API key) to read the failed run, reproduce the
  failure, and open a fix PR — or re-run/comment when it's an infra flake.

## Where the kakoune code lives

- `crates/vim/src/kakoune.rs` — the bulk of the implementation
- `assets/keymaps/vim.json` — the kakoune keymap sections
- `docs/src/kakoune.md` — user docs; keep in step with behavior changes
- Smaller integration points: `crates/vim/src/{vim.rs,state.rs,helix.rs,normal.rs,motion.rs,normal/search.rs}`,
  `crates/vim_mode_setting/`, `crates/zed/src/zed/quick_action_bar.rs`

## Rules of thumb

- Kakoune feature or fix → branch off `kakoune-mode`, PR base `kakoune-mode`.
  Keep commits small and rebase-friendly (they are replayed daily, forever)
  and prefix them `vim:` like the existing ones.
- CI/automation change → branch off `main`, PR base `main`.
- The test gate for anything that ends up on `kakoune-mode`:
  `cargo test -p vim && cargo build -p zed -p settings_ui`.
- A PR that would rewrite `kakoune-mode` history (e.g. a resolved rebase)
  cannot be merged through GitHub's UI — it must be applied with
  `git push --force-with-lease origin <branch>:kakoune-mode` after review.
- Upstream remote when needed:
  `git remote add upstream https://github.com/zed-industries/zed.git`
