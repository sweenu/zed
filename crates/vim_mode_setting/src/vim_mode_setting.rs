//! Contains the [`VimModeSetting`], [`HelixModeSetting`], and [`KakouneModeSetting`]
//! used to enable/disable Vim, Helix, and Kakoune modes.
//!
//! This is in its own crate as we want other crates to be able to enable or
//! disable Vim/Helix/Kakoune modes without having to depend on the `vim` crate
//! in its entirety.

use gpui::App;
use settings::{RegisterSetting, Settings, SettingsContent};

#[derive(RegisterSetting)]
pub struct VimModeSetting(pub bool);

impl Settings for VimModeSetting {
    fn from_settings(content: &SettingsContent) -> Self {
        Self(content.vim_mode.unwrap())
    }
}

impl VimModeSetting {
    pub fn is_enabled(cx: &App) -> bool {
        Self::try_get(cx)
            .map(|vim_mode| vim_mode.0)
            .unwrap_or(false)
    }
}

#[derive(RegisterSetting)]
pub struct HelixModeSetting(pub bool);

impl HelixModeSetting {
    pub fn is_enabled(cx: &App) -> bool {
        Self::try_get(cx)
            .map(|helix_mode| helix_mode.0)
            .unwrap_or(false)
    }
}

impl Settings for HelixModeSetting {
    fn from_settings(content: &SettingsContent) -> Self {
        Self(content.helix_mode.unwrap())
    }
}

#[derive(RegisterSetting)]
pub struct KakouneModeSetting(pub bool);

impl KakouneModeSetting {
    pub fn is_enabled(cx: &App) -> bool {
        Self::try_get(cx)
            .map(|kakoune_mode| kakoune_mode.0)
            .unwrap_or(false)
    }
}

impl Settings for KakouneModeSetting {
    fn from_settings(content: &SettingsContent) -> Self {
        Self(content.kakoune_mode.unwrap())
    }
}
