//! The printer states the UI displays, with the label, icon and colour token
//! for each. `web/packages/rust-client` generates its descriptor table from
//! [`PrinterDisplayState::ALL`] and [`PrinterDisplayState::descriptor`], so a
//! new state cannot reach the UI without all three.
//!
//! What the printer is doing ([`PrinterExecutionState`]) and whether its bed is
//! clear ([`BedClearState`]) stay separate facts. [`PrinterStateView`] carries
//! both next to the one-word summary.

use serde::Serialize;

/// Declares the display enum and [`PrinterDisplayState::ALL`] from one list,
/// so `ALL` cannot miss a variant.
macro_rules! display_states {
    ($($(#[$doc:meta])* $variant:ident,)+) => {
        /// One word and icon for a printer row.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
        #[serde(rename_all = "snake_case")]
        #[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
        pub enum PrinterDisplayState {
            $($(#[$doc])* $variant,)+
        }

        impl PrinterDisplayState {
            /// Every variant, in display order.
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];
        }
    };
}

display_states! {
    /// A job is running.
    Printing,
    /// Idle, but the last print may still be on the bed.
    BedToClear,
    /// Idle with a bed confirmed clear.
    Ready,
    /// A job is paused.
    Paused,
    /// The last job failed or the printer reports an error.
    Failed,
    /// The printer did not answer.
    Unreachable,
    /// The printer has no connection configured.
    NotSetUp,
    /// The printer is tracked by hand; nothing reports its state.
    Manual,
    /// The printer answered with a state Print Partner does not know.
    Unknown,
}

/// Lucide icon names. The UI maps each one to a component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub enum StatusIcon {
    Printer,
    PackageOpen,
    CircleCheck,
    CirclePause,
    OctagonAlert,
    WifiOff,
    CircleDashed,
    Hand,
    CircleQuestionMark,
}

/// Status colour tokens. The UI defines each as a `--status-*` CSS custom
/// property. Cyan is reserved for interaction (focus, selection, the primary
/// button), so no status token may use it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub enum StatusColor {
    #[serde(rename = "status-active")]
    Active,
    #[serde(rename = "status-positive")]
    Positive,
    #[serde(rename = "status-attention")]
    Attention,
    #[serde(rename = "status-danger")]
    Danger,
    #[serde(rename = "status-neutral")]
    Neutral,
}

/// How the UI renders one [`PrinterDisplayState`]: always an icon plus a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct PrinterDisplayDescriptor {
    pub state: PrinterDisplayState,
    pub label: &'static str,
    pub icon: StatusIcon,
    pub color: StatusColor,
}

impl PrinterDisplayState {
    /// The match has no wildcard arm: a variant without a descriptor does not
    /// compile.
    pub const fn descriptor(self) -> PrinterDisplayDescriptor {
        let (label, icon, color) = match self {
            Self::Printing => ("Printing", StatusIcon::Printer, StatusColor::Active),
            Self::BedToClear => (
                "Bed to clear",
                StatusIcon::PackageOpen,
                StatusColor::Attention,
            ),
            Self::Ready => ("Ready", StatusIcon::CircleCheck, StatusColor::Positive),
            Self::Paused => ("Paused", StatusIcon::CirclePause, StatusColor::Attention),
            Self::Failed => ("Failed", StatusIcon::OctagonAlert, StatusColor::Danger),
            Self::Unreachable => ("Unreachable", StatusIcon::WifiOff, StatusColor::Danger),
            Self::NotSetUp => ("Not set up", StatusIcon::CircleDashed, StatusColor::Neutral),
            Self::Manual => ("Manual", StatusIcon::Hand, StatusColor::Neutral),
            Self::Unknown => (
                "Unknown",
                StatusIcon::CircleQuestionMark,
                StatusColor::Neutral,
            ),
        };
        PrinterDisplayDescriptor {
            state: self,
            label,
            icon,
            color,
        }
    }

    /// Summarises the two facts into one word. An idle printer is Ready only
    /// when its bed is confirmed clear; an unknown bed shows Bed to clear so
    /// the operator confirms it first.
    pub const fn summarize(execution: PrinterExecutionState, bed_clear: BedClearState) -> Self {
        match (execution, bed_clear) {
            (PrinterExecutionState::Printing, _) => Self::Printing,
            (PrinterExecutionState::Paused, _) => Self::Paused,
            (PrinterExecutionState::Idle, BedClearState::Clear) => Self::Ready,
            (
                PrinterExecutionState::Idle,
                BedClearState::NeedsClearing | BedClearState::Unknown,
            ) => Self::BedToClear,
            (PrinterExecutionState::Failed, _) => Self::Failed,
            (PrinterExecutionState::Unreachable, _) => Self::Unreachable,
            (PrinterExecutionState::NotSetUp, _) => Self::NotSetUp,
            (PrinterExecutionState::Manual, _) => Self::Manual,
            (PrinterExecutionState::Unknown, _) => Self::Unknown,
        }
    }
}

/// What the printer is doing, or why Print Partner cannot tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub enum PrinterExecutionState {
    Printing,
    Paused,
    Idle,
    Failed,
    Unreachable,
    NotSetUp,
    Manual,
    Unknown,
}

/// Whether the bed is free for the next print. Only an operator confirmation
/// makes it `Clear`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub enum BedClearState {
    Clear,
    NeedsClearing,
    Unknown,
}

/// The printer state DTO: execution and bed-clear as separate fields, plus the
/// display summary derived from them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "codegen", derive(schemars::JsonSchema))]
pub struct PrinterStateView {
    execution: PrinterExecutionState,
    bed_clear: BedClearState,
    display: PrinterDisplayState,
}

impl PrinterStateView {
    pub const fn new(execution: PrinterExecutionState, bed_clear: BedClearState) -> Self {
        Self {
            execution,
            bed_clear,
            display: PrinterDisplayState::summarize(execution, bed_clear),
        }
    }

    pub const fn execution(&self) -> PrinterExecutionState {
        self.execution
    }

    pub const fn bed_clear(&self) -> BedClearState {
        self.bed_clear
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const EXECUTION: [PrinterExecutionState; 8] = [
        PrinterExecutionState::Printing,
        PrinterExecutionState::Paused,
        PrinterExecutionState::Idle,
        PrinterExecutionState::Failed,
        PrinterExecutionState::Unreachable,
        PrinterExecutionState::NotSetUp,
        PrinterExecutionState::Manual,
        PrinterExecutionState::Unknown,
    ];
    const BED: [BedClearState; 3] = [
        BedClearState::Clear,
        BedClearState::NeedsClearing,
        BedClearState::Unknown,
    ];

    /// The vocabulary in docs/product/ui-ux-target.md, in display order. The
    /// match has no wildcard, so a new variant fails to compile here too until
    /// the product vocabulary names it.
    fn product_word(state: PrinterDisplayState) -> &'static str {
        match state {
            PrinterDisplayState::Printing => "Printing",
            PrinterDisplayState::BedToClear => "Bed to clear",
            PrinterDisplayState::Ready => "Ready",
            PrinterDisplayState::Paused => "Paused",
            PrinterDisplayState::Failed => "Failed",
            PrinterDisplayState::Unreachable => "Unreachable",
            PrinterDisplayState::NotSetUp => "Not set up",
            PrinterDisplayState::Manual => "Manual",
            PrinterDisplayState::Unknown => "Unknown",
        }
    }

    #[test]
    fn printer_display_state_variants_are_the_product_vocabulary() {
        let words: Vec<_> = PrinterDisplayState::ALL
            .iter()
            .map(|state| product_word(*state))
            .collect();
        assert_eq!(
            words,
            [
                "Printing",
                "Bed to clear",
                "Ready",
                "Paused",
                "Failed",
                "Unreachable",
                "Not set up",
                "Manual",
                "Unknown"
            ]
        );
    }

    #[test]
    fn printer_display_state_every_variant_has_a_distinct_descriptor() {
        let mut icons = HashSet::new();
        for state in PrinterDisplayState::ALL {
            let descriptor = state.descriptor();
            assert_eq!(descriptor.state, *state);
            assert_eq!(descriptor.label, product_word(*state));
            assert!(icons.insert(descriptor.icon), "{state:?} reuses an icon");
        }
    }

    #[test]
    fn printer_display_state_colors_never_use_interaction_cyan() {
        for state in PrinterDisplayState::ALL {
            let wire = serde_json::to_value(state.descriptor().color).unwrap();
            let token = wire.as_str().unwrap();
            assert!(token.starts_with("status-"), "{state:?} uses {token}");
            for reserved in ["interaction", "cyan", "accent", "focus", "primary"] {
                assert!(!token.contains(reserved), "{state:?} uses {token}");
            }
        }
    }

    #[test]
    fn printer_display_state_serializes_in_snake_case() {
        let wire: Vec<_> = PrinterDisplayState::ALL
            .iter()
            .map(|state| serde_json::to_value(state).unwrap())
            .collect();
        assert_eq!(
            wire,
            [
                "printing",
                "bed_to_clear",
                "ready",
                "paused",
                "failed",
                "unreachable",
                "not_set_up",
                "manual",
                "unknown"
            ]
        );
    }

    #[test]
    fn printer_display_state_keeps_execution_and_bed_clear_separate() {
        let view =
            PrinterStateView::new(PrinterExecutionState::Failed, BedClearState::NeedsClearing);
        assert_eq!(
            serde_json::to_value(view).unwrap(),
            serde_json::json!({"execution": "failed", "bed_clear": "needs_clearing", "display": "failed"})
        );
        assert_eq!(view.execution(), PrinterExecutionState::Failed);
        assert_eq!(view.bed_clear(), BedClearState::NeedsClearing);
    }

    #[test]
    fn printer_display_state_summary_is_ready_only_with_a_confirmed_clear_bed() {
        let mut reached = HashSet::new();
        for execution in EXECUTION {
            for bed_clear in BED {
                let display = PrinterDisplayState::summarize(execution, bed_clear);
                let view =
                    serde_json::to_value(PrinterStateView::new(execution, bed_clear)).unwrap();
                assert_eq!(view["display"], serde_json::to_value(display).unwrap());
                reached.insert(display);
                let ready =
                    execution == PrinterExecutionState::Idle && bed_clear == BedClearState::Clear;
                assert_eq!(
                    display == PrinterDisplayState::Ready,
                    ready,
                    "{execution:?} {bed_clear:?}"
                );
            }
        }
        assert_eq!(
            PrinterDisplayState::summarize(PrinterExecutionState::Idle, BedClearState::Unknown),
            PrinterDisplayState::BedToClear
        );
        // Every display state is reachable from some pair of facts.
        assert_eq!(reached.len(), PrinterDisplayState::ALL.len());
    }
}
