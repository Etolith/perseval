use std::borrow::Cow;

use gpui::{AssetSource, SharedString, Svg, prelude::*, svg};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppIcon {
    Inbox,
    Runs,
    Compare,
    Evals,
    Sources,
    Settings,
    Database,
    Shield,
    Sparkles,
    Accessibility,
    ChevronDown,
    Plus,
    Pin,
}

impl AppIcon {
    const fn path(self, active: bool) -> &'static str {
        match (self, active) {
            (Self::Inbox, false) => "icons/inbox.svg",
            (Self::Inbox, true) => "icons/active/inbox.svg",
            (Self::Runs, false) => "icons/runs.svg",
            (Self::Runs, true) => "icons/active/runs.svg",
            (Self::Compare, false) => "icons/compare.svg",
            (Self::Compare, true) => "icons/active/compare.svg",
            (Self::Evals, false) => "icons/evals.svg",
            (Self::Evals, true) => "icons/active/evals.svg",
            (Self::Sources, false) => "icons/sources.svg",
            (Self::Sources, true) => "icons/active/sources.svg",
            (Self::Settings, false) => "icons/settings.svg",
            (Self::Settings, true) => "icons/active/settings.svg",
            (Self::Database, false) => "icons/database.svg",
            (Self::Database, true) => "icons/active/database.svg",
            (Self::Shield, false) => "icons/shield.svg",
            (Self::Shield, true) => "icons/active/shield.svg",
            (Self::Sparkles, false) => "icons/sparkles.svg",
            (Self::Sparkles, true) => "icons/active/sparkles.svg",
            (Self::Accessibility, false) => "icons/accessibility.svg",
            (Self::Accessibility, true) => "icons/active/accessibility.svg",
            (Self::ChevronDown, false) => "icons/chevron-down.svg",
            (Self::ChevronDown, true) => "icons/active/chevron-down.svg",
            (Self::Plus, false) => "icons/plus.svg",
            (Self::Plus, true) => "icons/active/plus.svg",
            (Self::Pin, false) => "icons/pin.svg",
            (Self::Pin, true) => "icons/active/pin.svg",
        }
    }
}

pub(crate) fn icon(icon: AppIcon, size: f32, active: bool) -> Svg {
    svg()
        .path(icon.path(active))
        .size(gpui::px(size))
        // GPUI only paints an SVG when the SVG element itself has a color.
        .text_color(if active {
            gpui::rgb(0x58c8d3)
        } else {
            gpui::rgb(0x93a0ad)
        })
}

pub(crate) struct PersevalAssets;

impl AssetSource for PersevalAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let (path, active) = match path.strip_prefix("icons/active/") {
            Some(name) => (format!("icons/{name}"), true),
            None => (path.to_owned(), false),
        };
        let bytes: Option<&'static [u8]> = match path.as_str() {
            "icons/inbox.svg" => Some(include_bytes!("../assets/icons/inbox.svg")),
            "icons/runs.svg" => Some(include_bytes!("../assets/icons/runs.svg")),
            "icons/compare.svg" => Some(include_bytes!("../assets/icons/compare.svg")),
            "icons/evals.svg" => Some(include_bytes!("../assets/icons/evals.svg")),
            "icons/sources.svg" => Some(include_bytes!("../assets/icons/sources.svg")),
            "icons/settings.svg" => Some(include_bytes!("../assets/icons/settings.svg")),
            "icons/database.svg" => Some(include_bytes!("../assets/icons/database.svg")),
            "icons/shield.svg" => Some(include_bytes!("../assets/icons/shield.svg")),
            "icons/sparkles.svg" => Some(include_bytes!("../assets/icons/sparkles.svg")),
            "icons/accessibility.svg" => Some(include_bytes!("../assets/icons/accessibility.svg")),
            "icons/chevron-down.svg" => Some(include_bytes!("../assets/icons/chevron-down.svg")),
            "icons/plus.svg" => Some(include_bytes!("../assets/icons/plus.svg")),
            "icons/pin.svg" => Some(include_bytes!("../assets/icons/pin.svg")),
            _ => None,
        };
        Ok(bytes.map(|bytes| {
            if active {
                Cow::Owned(
                    String::from_utf8_lossy(bytes)
                        .replace("#93a0ad", "#58c8d3")
                        .into_bytes(),
                )
            } else {
                Cow::Borrowed(bytes)
            }
        }))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        if path != "icons" {
            return Ok(Vec::new());
        }
        Ok([
            "inbox.svg",
            "runs.svg",
            "compare.svg",
            "evals.svg",
            "sources.svg",
            "settings.svg",
            "database.svg",
            "shield.svg",
            "sparkles.svg",
            "accessibility.svg",
            "chevron-down.svg",
            "plus.svg",
            "pin.svg",
        ]
        .into_iter()
        .map(SharedString::from)
        .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ICONS: [AppIcon; 13] = [
        AppIcon::Inbox,
        AppIcon::Runs,
        AppIcon::Compare,
        AppIcon::Evals,
        AppIcon::Sources,
        AppIcon::Settings,
        AppIcon::Database,
        AppIcon::Shield,
        AppIcon::Sparkles,
        AppIcon::Accessibility,
        AppIcon::ChevronDown,
        AppIcon::Plus,
        AppIcon::Pin,
    ];

    #[test]
    fn every_icon_has_muted_and_active_bundled_assets() {
        let assets = PersevalAssets;
        for icon in ICONS {
            let muted = assets
                .load(icon.path(false))
                .expect("load muted icon")
                .expect("muted icon exists");
            let active = assets
                .load(icon.path(true))
                .expect("load active icon")
                .expect("active icon exists");

            assert!(String::from_utf8_lossy(&muted).contains("#93a0ad"));
            assert!(String::from_utf8_lossy(&active).contains("#58c8d3"));
        }
    }
}
