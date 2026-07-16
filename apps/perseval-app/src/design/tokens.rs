use gpui::{Rgba, Window};

pub const fn color(hex: u32) -> Rgba {
    Rgba {
        r: ((hex >> 16) & 0xff) as f32 / 255.,
        g: ((hex >> 8) & 0xff) as f32 / 255.,
        b: (hex & 0xff) as f32 / 255.,
        a: 1.,
    }
}

/// Semantic palette shared by every Perseval screen.
pub struct Theme;

impl Theme {
    // Foundational surfaces. Keep aliases below while existing screens migrate,
    // but new components should choose the role that matches their purpose.
    pub const APPLICATION_BACKGROUND: Rgba = color(0x101418);
    pub const PANEL_SURFACE: Rgba = color(0x151a20);
    pub const ELEVATED_SURFACE: Rgba = color(0x1a2027);
    pub const TOOLBAR_SURFACE: Rgba = color(0x12171c);
    pub const INSPECTOR_SURFACE: Rgba = color(0x13191f);
    pub const ROW_HOVER: Rgba = color(0x192128);
    pub const ROW_SELECTED: Rgba = color(0x173339);
    pub const ROW_SELECTED_STRONG: Rgba = color(0x1b3c42);
    pub const EVIDENCE_SURFACE: Rgba = color(0x2b2416);
    pub const EVIDENCE_SELECTED_SURFACE: Rgba = color(0x3a2d16);
    pub const DISABLED_SURFACE: Rgba = color(0x171b20);

    pub const BG: Rgba = Self::APPLICATION_BACKGROUND;
    pub const PANEL: Rgba = Self::PANEL_SURFACE;
    pub const PANEL_ALT: Rgba = Self::ELEVATED_SURFACE;
    pub const BORDER: Rgba = color(0x282e37);
    pub const TEXT: Rgba = color(0xe7e9ed);
    pub const MUTED: Rgba = color(0xaab2bf);
    pub const DIM: Rgba = color(0x7f8896);
    pub const CYAN: Rgba = color(0x54c7d9);
    pub const PURPLE: Rgba = color(0xa78bfa);
    pub const GREEN: Rgba = color(0x58d68d);
    pub const AMBER: Rgba = color(0xd9a851);
    pub const RED: Rgba = color(0xff6678);

    // Interactive and status surfaces. These names describe intent rather than
    // one screen's current treatment, so contrast and future themes stay
    // centralized.
    pub const TEXT_ON_ACCENT: Rgba = color(0x071014);
    pub const FOCUS_RING: Rgba = Self::CYAN;
    pub const PRIMARY_ACTION: Rgba = Self::CYAN;
    pub const SECONDARY_ACTION_SURFACE: Rgba = Self::ELEVATED_SURFACE;
    pub const PRESSED_SURFACE: Rgba = color(0x20424a);
    pub const ACCENT_MUTED: Rgba = Self::ROW_SELECTED;
    pub const SELECTED: Rgba = Self::ROW_SELECTED_STRONG;
    pub const SELECTED_SUBTLE: Rgba = Self::ROW_HOVER;
    pub const INFO_SURFACE: Rgba = color(0x10232b);
    pub const SUCCESS_SURFACE: Rgba = color(0x10231a);
    pub const WARNING_SURFACE: Rgba = color(0x2a2113);
    pub const DANGER_SURFACE: Rgba = color(0x2a1519);
    pub const INSET_SURFACE: Rgba = Self::INSPECTOR_SURFACE;
    pub const OVERLAY: Rgba = color(0x05090d);

    // Execution roles. These are categorical, not decorative: the same role
    // keeps the same tint in Investigation, Tree, Timeline, Compare, and the
    // inspector.
    pub const AGENT_PLANNER: Rgba = color(0xa78bfa);
    pub const AGENT_BROWSER: Rgba = Self::CYAN;
    pub const AGENT_VERIFIER: Rgba = Self::GREEN;
    pub const AGENT_TOOL: Rgba = color(0xaab2bf);
    pub const AGENT_MODEL: Rgba = color(0x8da4d8);
    pub const EXACT_EVIDENCE: Rgba = Self::AMBER;
    pub const FAILURE: Rgba = Self::RED;
    pub const VERIFIED_SUCCESS: Rgba = Self::GREEN;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionRole {
    Planner,
    Browser,
    Verifier,
    Tool,
    Model,
    Evidence,
    Failure,
    Success,
}

impl ExecutionRole {
    pub const fn tint(self) -> Rgba {
        match self {
            Self::Planner => Theme::AGENT_PLANNER,
            Self::Browser => Theme::AGENT_BROWSER,
            Self::Verifier => Theme::AGENT_VERIFIER,
            Self::Tool => Theme::AGENT_TOOL,
            Self::Model => Theme::AGENT_MODEL,
            Self::Evidence => Theme::EXACT_EVIDENCE,
            Self::Failure => Theme::FAILURE,
            Self::Success => Theme::VERIFIED_SUCCESS,
        }
    }

    pub const fn surface(self) -> Rgba {
        match self {
            Self::Evidence => Theme::EVIDENCE_SURFACE,
            Self::Failure => Theme::DANGER_SURFACE,
            Self::Success | Self::Verifier => Theme::SUCCESS_SURFACE,
            Self::Planner | Self::Browser | Self::Tool | Self::Model => Theme::ELEVATED_SURFACE,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Breakpoint {
    Compact,
    Standard,
    Wide,
}

impl Breakpoint {
    pub fn for_width(width: f32) -> Self {
        if width < 960. {
            Self::Compact
        } else if width < 1_360. {
            Self::Standard
        } else {
            Self::Wide
        }
    }

    /// Resolve layout from the space that remains after the user's text scale
    /// is applied. A 1,080 px window at 200% must not keep a desktop table.
    pub fn for_window(window: &Window) -> Self {
        let width: f32 = window.viewport_size().width.into();
        let rem_size: f32 = window.rem_size().into();
        let text_scale = (rem_size / 16.).max(1.);
        Self::for_width(width / text_scale)
    }
}

pub struct Spacing;

impl Spacing {
    pub const XS: f32 = 4.;
    pub const SM: f32 = 8.;
    pub const MD: f32 = 12.;
    pub const LG: f32 = 16.;
    pub const XL: f32 = 24.;
    pub const XXL: f32 = 32.;
    pub const XXXL: f32 = 48.;
}

pub struct ControlSize;

impl ControlSize {
    pub const COMPACT: f32 = 28.;
    pub const DEFAULT: f32 = 32.;
    pub const PRIMARY: f32 = 36.;
    pub const ROW: f32 = 40.;
}

/// Repeated workbench geometry. One-off visualization dimensions stay local.
pub struct Geometry;

impl Geometry {
    pub const ACTIVITY_RAIL_WIDTH: f32 = 48.;
    pub const TOP_BAR_HEIGHT: f32 = 44.;
    pub const TAB_STRIP_HEIGHT: f32 = 40.;
    pub const PAGE_GUTTER: f32 = 24.;
    pub const TABLE_ROW_COMPACT: f32 = 36.;
    pub const TABLE_ROW_STANDARD: f32 = 44.;
    pub const TRACE_ROW: f32 = 48.;
    pub const TOOLBAR_HEIGHT: f32 = 44.;
    pub const STICKY_ACTION_BAR_HEIGHT: f32 = 60.;
    pub const INSPECTOR_MIN_WIDTH: f32 = 280.;
    pub const INSPECTOR_DEFAULT_WIDTH: f32 = 360.;
    pub const INSPECTOR_MAX_WIDTH: f32 = 640.;
    pub const TREE_INDENT: f32 = 18.;
    pub const RADIUS_COMPACT: f32 = 4.;
    pub const RADIUS_STANDARD: f32 = 6.;
}

/// Fixed product-UI type roles. Explanations use the UI face; callers apply a
/// monospace face only to machine-readable values.
pub struct Typography;

impl Typography {
    pub const PAGE_TITLE: f32 = 20.;
    pub const SCREEN_DESCRIPTION: f32 = 14.;
    pub const SECTION_TITLE: f32 = 14.;
    pub const BODY: f32 = 13.;
    pub const TABLE_HEADER: f32 = 11.;
    pub const TABLE_PRIMARY: f32 = 12.;
    pub const TABLE_SECONDARY: f32 = 11.;
    pub const METADATA: f32 = 11.;
    pub const MACHINE_IDENTIFIER: f32 = 11.;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakpoint_contract_matches_the_ux_plan() {
        assert_eq!(Breakpoint::for_width(720.), Breakpoint::Compact);
        assert_eq!(Breakpoint::for_width(959.), Breakpoint::Compact);
        assert_eq!(Breakpoint::for_width(960.), Breakpoint::Standard);
        assert_eq!(Breakpoint::for_width(1_359.), Breakpoint::Standard);
        assert_eq!(Breakpoint::for_width(1_360.), Breakpoint::Wide);
    }

    #[test]
    fn semantic_foregrounds_meet_wcag_aa_on_dark_surfaces() {
        for (label, foreground) in [
            ("text", Theme::TEXT),
            ("muted", Theme::MUTED),
            ("dim", Theme::DIM),
            ("cyan", Theme::CYAN),
            ("purple", Theme::PURPLE),
            ("green", Theme::GREEN),
            ("amber", Theme::AMBER),
            ("red", Theme::RED),
        ] {
            assert!(
                contrast_ratio(foreground, Theme::PANEL_ALT) >= 4.5,
                "{label} must retain 4.5:1 contrast on the lightest dark panel"
            );
        }
    }

    #[test]
    fn accent_foreground_meets_wcag_aa() {
        assert!(contrast_ratio(Theme::TEXT_ON_ACCENT, Theme::CYAN) >= 4.5);
    }

    #[test]
    fn stateful_surface_foregrounds_meet_wcag_aa() {
        for (surface_label, surface) in [
            ("application", Theme::APPLICATION_BACKGROUND),
            ("panel", Theme::PANEL_SURFACE),
            ("elevated", Theme::ELEVATED_SURFACE),
            ("toolbar", Theme::TOOLBAR_SURFACE),
            ("inspector", Theme::INSPECTOR_SURFACE),
            ("row hover", Theme::ROW_HOVER),
            ("row selected", Theme::ROW_SELECTED),
            ("row selected strong", Theme::ROW_SELECTED_STRONG),
            ("evidence", Theme::EVIDENCE_SURFACE),
            ("evidence selected", Theme::EVIDENCE_SELECTED_SURFACE),
            ("disabled", Theme::DISABLED_SURFACE),
            ("selected", Theme::SELECTED),
            ("selected subtle", Theme::SELECTED_SUBTLE),
            ("information", Theme::INFO_SURFACE),
            ("success", Theme::SUCCESS_SURFACE),
            ("warning", Theme::WARNING_SURFACE),
            ("danger", Theme::DANGER_SURFACE),
            ("inset", Theme::INSET_SURFACE),
        ] {
            for (foreground_label, foreground) in [("text", Theme::TEXT), ("muted", Theme::MUTED)] {
                assert!(
                    contrast_ratio(foreground, surface) >= 4.5,
                    "{foreground_label} must retain 4.5:1 contrast on {surface_label} surfaces"
                );
            }
        }
    }

    #[test]
    fn execution_role_tints_are_legible_on_their_surfaces() {
        for role in [
            ExecutionRole::Planner,
            ExecutionRole::Browser,
            ExecutionRole::Verifier,
            ExecutionRole::Tool,
            ExecutionRole::Model,
            ExecutionRole::Evidence,
            ExecutionRole::Failure,
            ExecutionRole::Success,
        ] {
            assert!(
                contrast_ratio(role.tint(), role.surface()) >= 4.5,
                "{role:?} must retain 4.5:1 contrast on its semantic surface"
            );
        }
    }

    fn contrast_ratio(left: Rgba, right: Rgba) -> f32 {
        let (light, dark) = {
            let left = luminance(left);
            let right = luminance(right);
            if left >= right {
                (left, right)
            } else {
                (right, left)
            }
        };
        (light + 0.05) / (dark + 0.05)
    }

    fn luminance(color: Rgba) -> f32 {
        0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
    }

    fn linear(channel: f32) -> f32 {
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    }
}
