//! Kakoune mode.
//!
//! Like Helix mode (which was inspired by Kakoune), this is a selection-first
//! editing mode built on top of the vim infrastructure. Unlike Helix, Kakoune
//! has no select mode: selections are extended per-keystroke with
//! Shift-modified movement keys instead.

use editor::Editor;
use gpui::{Context, Window};

use crate::{Vim, motion::Motion};

pub fn register(_editor: &mut Editor, _cx: &mut Context<Vim>) {}

impl Vim {
    pub fn kakoune_motion(
        &mut self,
        motion: Motion,
        times: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Placeholder until kakoune-specific motions land: helix shares
        // Kakoune's selection-first cursor model, so its non-extending
        // motions are a close approximation.
        self.helix_normal_motion(motion, times, window, cx);
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
}
