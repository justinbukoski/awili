//! U4f — the Ask/Auto mode chip, its menu, and the Auto confirmation.
//!
//! The pure core is [`AskMenu`]/[`choose`]: Ask applies immediately, Auto
//! needs one explicit confirmation (D7) and is refused for over-broad folders
//! (D8). The `impl AwiliApp` block owns the render side: the chip (styled like
//! the executor chip — [`crate::AwiliApp::ask_mode_chip`]), the Work chip
//! ([`crate::AwiliApp::work_ask_mode_chip`]), the popover
//! ([`crate::AwiliApp::ask_mode_popover`]) and the mutations
//! ([`crate::AwiliApp::set_ask_mode`]). The menu state itself lives on [`crate::work::WorkState::ask_menu`].

#[cfg(test)]
use awili_code::CodeEngine;
use awili_work::ApprovalMode;
use gpui::{div, prelude::*, AnyElement, Context, SharedString};

use crate::ask_mode::{
    auto_confirm_body, auto_refusal, effective, work_support, AskKey, AskSupport,
    AUTO_CONFIRM_TITLE, KEEP_ASKING, SWITCH_TO_AUTO,
};
use crate::icons::Icon;
use crate::theme;

/// RC2-UX (D-R1 M9b): the test lookup key of the open Ask/Auto menu card.
pub(crate) const ASK_MENU_CARD_SELECTOR: &str = "rc2ux-ask-menu-card";
const ASK_MODE_ASK_ROW_SELECTOR: &str = "ask-mode-row-0";
const ASK_MODE_AUTO_ROW_SELECTOR: &str = "ask-mode-row-1";

/// The state of the Ask/Auto mode menu (U4f, pure). At most one menu is open
/// at a time: the chip click opens it for its key, and the Auto row there
/// escalates to the confirmation for the same key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum AskMenu {
    /// No menu open.
    #[default]
    Closed,
    /// The two-row Ask/Auto menu is open for `key`.
    Open(AskKey),
    /// The Auto confirmation is open for `key` (the menu escalated to it).
    ConfirmAuto(AskKey),
}

/// What one pick against an open menu does (U4f, pure).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MenuStep {
    /// Apply `mode` to `key` (Ask: immediately; Auto: after the confirm).
    SetMode(AskKey, ApprovalMode),
    /// Show the Auto confirmation for `key`.
    Confirm(AskKey),
    /// Refuse Auto and show `msg` as a toast (D8); the mode is unchanged.
    Refuse(String),
    /// The pick does not apply to the menu's state; nothing happens.
    Nothing,
}

/// Apply a pick (`pick`) to the menu (`menu`), folding the key's `refusal`
/// (from [`auto_refusal`]) into the Auto path (U4f, pure):
/// - From [`AskMenu::Open`]: Ask applies immediately; Auto with a `refusal`
///   refuses it (the toast says why); Auto otherwise escalates to
///   [`AskMenu::ConfirmAuto`].
/// - From [`AskMenu::ConfirmAuto`]: the confirm applies Auto; any other pick
///   does nothing.
/// - [`AskMenu::Closed`] does nothing at all.
pub(crate) fn choose(menu: &AskMenu, pick: ApprovalMode, refusal: Option<String>) -> MenuStep {
    match menu {
        AskMenu::Open(key) => match pick {
            ApprovalMode::AskBeforeChanges => MenuStep::SetMode(key.clone(), pick),
            ApprovalMode::Auto => match refusal {
                Some(msg) => MenuStep::Refuse(msg),
                None => MenuStep::Confirm(key.clone()),
            },
        },
        AskMenu::ConfirmAuto(key) => match pick {
            // The confirmation offers only Auto; anything else (e.g. Ask)
            // does not apply to it.
            ApprovalMode::Auto => MenuStep::SetMode(key.clone(), pick),
            _ => MenuStep::Nothing,
        },
        AskMenu::Closed => MenuStep::Nothing,
    }
}

/// The menu subtitle of "Ask before changes" (spec D1, exact copy).
pub(crate) const ASK_SUBTITLE: &str = "Stops for your OK before files change or commands run.";
/// The menu subtitle of "Auto" (spec D1, exact copy).
pub(crate) const AUTO_SUBTITLE: &str = "Runs without asking.";

/// The asking row's subtitle for a native harness: it plans first and does
/// not prompt per tool call.
pub(crate) const PLAN_FIRST_SUBTITLE: &str =
    "Looks around read-only. Use the plan's approval button to run actions; approval in chat does not grant tool access.";

/// The two menu rows (mode, label, subtitle) in order (spec D1).
#[cfg(test)]
pub(crate) fn menu_rows() -> [(ApprovalMode, &'static str, &'static str); 2] {
    menu_rows_for(AskSupport::PerAction)
}

/// The same two rows, saying what the asking row does for the target the next
/// turn will use. The stored choice and its permissions are unchanged.
pub(crate) fn menu_rows_for(
    support: AskSupport,
) -> [(ApprovalMode, &'static str, &'static str); 2] {
    let (label, sub) = if support == AskSupport::PlanFirst {
        ("Plan first", PLAN_FIRST_SUBTITLE)
    } else {
        ("Ask before changes", ASK_SUBTITLE)
    };
    [
        (ApprovalMode::AskBeforeChanges, label, sub),
        (ApprovalMode::Auto, "Auto", AUTO_SUBTITLE),
    ]
}

/// What a key's NEXT turn will do about asking, from one source for the chip
/// and the menu: the conversation's saved next-turn target, never the running
/// turn and never an ambient executor.
pub(crate) struct NextAsk {
    pub(crate) support: AskSupport,
    /// The chip's label for the stored choice folded with the target.
    #[cfg(test)]
    pub(crate) chip_label: &'static str,
    /// The asking row's label in the menu.
    #[cfg(test)]
    pub(crate) menu_label: &'static str,
}

/// R1i: the chip's icon. A lock means "Awili will ask before files change":
/// R1i-FIX1: exactly the postures [`effective`] folds to an asking style
/// (`AskPerAction`, `AskPlanFirst` — the two `chip_text` calls "Ask before
/// changes"). Auto and every `NoAsk` get the bolt, the Fleet Gateway and
/// Build included: Awili can't hold their file changes for approval (RC1
/// round 1: "The lock icon next to 'Runs without asking' suggests protection
/// while meaning the opposite"). No wildcard: a new posture must choose.
pub(crate) fn chip_icon(eff: crate::ask_mode::Effective) -> Icon {
    use crate::ask_mode::Effective;
    match eff {
        Effective::AskPerAction | Effective::AskPlanFirst => Icon::Lock,
        Effective::Auto | Effective::NoAsk(_) => Icon::Zap,
    }
}

/// The posture in a narrow top bar; the accessible name and tooltip keep the
/// full words.
fn short_label(label: &str) -> &str {
    match label {
        "Ask before changes" => "Ask",
        "Plan first" => "Plan",
        "Asks before risky commands" => "Asks",
        "Runs without asking" => "No ask",
        other => other,
    }
}

/// R1i-FIX1: the mode chip's `debug_selector`, so a test can click the
/// rendered chip.
pub(crate) const ASK_MODE_CHIP_SELECTOR: &str = "ask-mode-chip";

/// Shown on the consent chip while the conversation has no usable next target.
const NEEDS_MODEL_TOOLTIP: &str =
    "Whether Awili asks first depends on the model. Choose one for your next message.";

/// How the mode chip looks and behaves (U4f-FIX1, pure).
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ChipLook {
    /// The label colour.
    pub text: gpui::Rgba,
    /// The icon colour.
    pub icon: gpui::Rgba,
    /// Whether a click opens the menu.
    pub clickable: bool,
}

/// The chip's look: while a run is in flight for the key it is dimmed
/// (`theme::p().faint`, like the executor chip) and inert; otherwise a `warn`
/// posture is in `theme::p().chip_warn`, and only a switchable posture is
/// clickable. The tooltip is shown in every case.
#[cfg(test)]
pub(crate) fn chip_look(warn: bool, switchable: bool, running: bool) -> ChipLook {
    let colour = if running {
        theme::p().faint
    } else if warn {
        theme::p().chip_warn
    } else {
        theme::p().chip_text
    };
    ChipLook {
        text: colour,
        icon: colour,
        clickable: switchable && !running,
    }
}

impl crate::AwiliApp {
    /// The Ask/Auto mode chip for `key` + `support`, styled like the executor
    /// chip (the `chips_row` pattern, `main.rs`): a `Lock` (Ask) or `Zap`
    /// (Auto) icon, the `chip_text` label, and the tooltip. `warn` postures
    /// render in `theme::p().chip_warn`. Every chip carries its tooltip. When
    /// `switchable`, a click opens the menu for `key`; while a run is in
    /// flight for `key` it is dimmed and inert ([`chip_look`]).
    pub(crate) fn ask_mode_chip(
        &self,
        key: AskKey,
        support: AskSupport,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> crate::ui::Chip {
        let eff = effective(self.work.ask_modes.mode_for(&key), support);
        let text = crate::ask_mode::chip_text(eff);
        let running = self.ask_key_running(&key);
        let icon = chip_icon(eff);
        // The chip lives in the top bar: the posture alone (short in a narrow
        // bar) with a caret when it opens the menu; the accessible name keeps
        // the full words.
        let shown = if narrow {
            short_label(text.label)
        } else {
            text.label
        };
        let shown = if text.switchable {
            format!("{shown} \u{25be}")
        } else {
            shown.to_string()
        };
        let chip = crate::ui::Chip::new("ask-mode", shown)
            .label(format!("Mode: {}", text.label))
            .selector(ASK_MODE_CHIP_SELECTOR)
            .tab_index(crate::focus_nav::EXECUTOR)
            .tooltip(text.tooltip)
            .icon(icon)
            .disabled_if(
                running
                    .then_some("Stop the current turn before changing its consent choice".into()),
            )
            .on_click(cx.listener(move |app, _, _, cx| {
                if text.switchable {
                    app.work.ask_menu = AskMenu::Open(key.clone());
                    cx.notify();
                } else {
                    app.explain_ask_mode_chip(eff, cx);
                }
            }));
        match eff {
            crate::ask_mode::Effective::AskPerAction | crate::ask_mode::Effective::AskPlanFirst => {
                chip.outlined(theme::Tone::Attention)
            }
            crate::ask_mode::Effective::Auto => chip.outlined(theme::Tone::Contrast),
            crate::ask_mode::Effective::NoAsk(_) => chip,
        }
    }

    /// R1i: the click on a chip that cannot switch: its explanation (the
    /// tooltip text) in a toast (RC1-H7: an information toast, it goes by
    /// itself after [`crate::INFO_TOAST_SECS`]). A switchable posture opens
    /// the menu instead and never comes here.
    pub(crate) fn explain_ask_mode_chip(
        &mut self,
        eff: crate::ask_mode::Effective,
        cx: &mut Context<Self>,
    ) {
        let text = crate::ask_mode::chip_text(eff);
        if !text.switchable {
            self.show_info_toast(text.tooltip, cx);
        }
    }

    /// The same per-owner consent choice, in the shared floating composer.
    pub(crate) fn composer_ask_mode_chip(
        &self,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> Option<crate::ui::Chip> {
        match self.mode {
            crate::work::Mode::Work => Some(self.work_ask_mode_chip(narrow, cx)),
            crate::work::Mode::Code => {
                let tab = self.work.code_tabs.get(self.work.code_active)?;
                Some(self.ask_mode_chip_for_next_target(AskKey::CodeTab(tab.id), narrow, cx))
            }
            _ => None,
        }
    }

    /// The Work-mode composer chip: the current Work conversation's key and
    /// the support of that conversation's next-turn target.
    pub(crate) fn work_ask_mode_chip(
        &self,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> crate::ui::Chip {
        let key = self.work_chip_key();
        self.ask_mode_chip_for_next_target(key, narrow, cx)
    }

    /// `key`'s next turn, from its conversation's saved next-turn target. `None`
    /// when that target is unavailable: nothing is promised from an ambient
    /// model.
    pub(crate) fn next_ask(&self, key: &AskKey) -> Option<NextAsk> {
        let owner = match key {
            AskKey::Work(id) => crate::workshop_composer::ModelOwner::Work(id.clone()),
            AskKey::CodeTab(id) => crate::workshop_composer::ModelOwner::Native(*id),
        };
        let target = self.composer_target_for(&owner)?;
        let support = work_support(&target, self.work_engine_kind());
        Some(NextAsk {
            support,
            #[cfg(test)]
            chip_label: crate::ask_mode::chip_text(effective(
                self.work.ask_modes.mode_for(key),
                support,
            ))
            .label,
            #[cfg(test)]
            menu_label: menu_rows_for(support)[0].1,
        })
    }

    /// The consent chip for `key`'s NEXT turn. A running turn keeps its own
    /// approval and Stop; the stored Ask/Auto choice is only shown, never
    /// changed. With no usable target the chip is inert and promises nothing.
    pub(crate) fn ask_mode_chip_for_next_target(
        &self,
        key: AskKey,
        narrow: bool,
        cx: &mut Context<Self>,
    ) -> crate::ui::Chip {
        match self.next_ask(&key) {
            Some(next) => self.ask_mode_chip(key, next.support, narrow, cx),
            None => crate::ui::Chip::new("ask-mode", "Choose a model")
                .selector(ASK_MODE_CHIP_SELECTOR)
                .tab_index(crate::focus_nav::EXECUTOR)
                .tooltip(NEEDS_MODEL_TOOLTIP)
                .disabled(NEEDS_MODEL_TOOLTIP),
        }
    }

    /// The Work chip's key (U4e1-FIX2): the conversation the NEXT submission
    /// runs in — `submit_work_run` dispatches `sidebar.active_id` — never the
    /// last run's `work.conv_id`, so the chip shows and changes the choice
    /// the next dispatch will use.
    pub(crate) fn work_chip_key(&self) -> AskKey {
        AskKey::Work(self.sidebar.active_id().to_string())
    }

    /// The refusal (D8) for making THIS conversation Auto: the key's folder
    /// (the Work folder for Work keys, the active Code project's folder —
    /// `code_cwd()` — for Code keys), then the key's [`auto_refusal`]
    /// (`$HOME`, `/` or no folder refuse Auto).
    fn ask_menu_refusal(&self, key: &AskKey) -> Option<String> {
        let folder: Option<String> = match key {
            AskKey::Work(_) => self.work.folder.clone().map(|p| p.display().to_string()),
            AskKey::CodeTab(_) => Some(self.code_cwd().display().to_string()),
        };
        auto_refusal(folder.as_deref())
    }

    /// Whether a run is in flight for `key` (the chip is dimmed and inert
    /// then, like the executor chip): a Work turn for the key's conversation,
    /// or a Code turn streaming in the key's tab.
    fn ask_key_running(&self, key: &AskKey) -> bool {
        match key {
            AskKey::Work(id) => {
                self.work.running && self.work.conv_id.as_deref() == Some(id.as_str())
            }
            AskKey::CodeTab(id) => self
                .work
                .code_tabs
                .iter()
                .any(|t| t.id == *id && t.streaming),
        }
    }

    /// The Ask/Auto popover, shown while a menu is open (the root overlay
    /// list, next to the executor popover): `Open` renders the two rows
    /// (label + subtitle, a check on the current mode); `ConfirmAuto` renders
    /// the title, the `auto_confirm_body` and the two pills. Same scrim/card
    /// mechanics as the executor popover — the scrim closes the menu.
    pub(crate) fn ask_mode_popover(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let key = match &self.work.ask_menu {
            AskMenu::Open(k) => k.clone(),
            AskMenu::ConfirmAuto(k) => k.clone(),
            AskMenu::Closed => return None,
        };
        let next = self.next_ask(&key)?;
        if matches!(next.support, AskSupport::Unavailable(_)) {
            return None;
        }
        let support = next.support;
        let inner: AnyElement = if matches!(self.work.ask_menu, AskMenu::ConfirmAuto(_)) {
            let keep = crate::ui::Button::new("ask-auto-keep", KEEP_ASKING)
                .selector("ask-auto-keep")
                .ghost()
                .on_click(cx.listener(|this, _ev, _w, cx| {
                    this.work.ask_menu = AskMenu::Closed;
                    cx.notify();
                }));
            let confirm = crate::ui::Button::new("ask-auto-confirm", SWITCH_TO_AUTO)
                .selector("ask-auto-confirm")
                .primary()
                .on_click(cx.listener(move |this, _ev, _w, cx| {
                    if let AskMenu::ConfirmAuto(key) = &this.work.ask_menu {
                        this.set_ask_mode(key.clone(), ApprovalMode::Auto, cx);
                    }
                }));
            crate::ui::Card::new("ask-mode-card-content")
                .selector(ASK_MENU_CARD_SELECTOR)
                .children([div()
                    .flex()
                    .flex_col()
                    .gap(theme::z(theme::SP_8))
                    .child(
                        crate::ui::label(AUTO_CONFIRM_TITLE)
                            .strong()
                            .text_color_override(theme::p().chip_warn)
                            .wrap(),
                    )
                    .child(
                        crate::ui::label(auto_confirm_body(support))
                            .text_color_override(theme::p().text)
                            .wrap(),
                    )])
                .actions([keep, confirm])
                .into_any_element()
        } else {
            let mode = self.work.ask_modes.mode_for(&key);
            let rows = menu_rows_for(support);
            let refusal = self.ask_menu_refusal(&key);
            let col = rows
                .iter()
                .enumerate()
                .map(|(i, (m, label, sub))| (i, *m, *label, *sub, key.clone()))
                .fold(
                    div().flex().flex_col().gap(theme::z(theme::SP_4)),
                    |col, (i, m, label, sub, row_key)| {
                        let selected = m == mode;
                        let selector = if i == 0 {
                            ASK_MODE_ASK_ROW_SELECTOR
                        } else {
                            ASK_MODE_AUTO_ROW_SELECTOR
                        };
                        let row =
                            crate::ui::Row::new(SharedString::from(format!("ask-mode-row-{i}")))
                                .selector(selector)
                                .title(label)
                                .subtitle(sub)
                                .wrap()
                                .selected(selected)
                                .on_click(cx.listener(move |this, _ev, _w, cx| {
                                    let refusal = this.ask_menu_refusal(&row_key);
                                    match choose(&this.work.ask_menu, m, refusal) {
                                        MenuStep::SetMode(k, mode) => {
                                            this.set_ask_mode(k, mode, cx)
                                        }
                                        MenuStep::Confirm(k) => {
                                            this.work.ask_menu = AskMenu::ConfirmAuto(k);
                                            cx.notify();
                                        }
                                        MenuStep::Refuse(msg) => {
                                            this.show_toast(msg, cx);
                                        }
                                        MenuStep::Nothing => {}
                                    }
                                }));
                        let row = if selected {
                            row.trailing(crate::ui::label("\u{2713}"))
                        } else {
                            row
                        };
                        col.child(row)
                    },
                );
            let col = col.children(refusal.map(|reason| {
                div()
                    .flex()
                    .flex_col()
                    .gap(theme::z(theme::SP_8))
                    .child(crate::ui::label(reason).wrap())
                    .child(
                        crate::ui::Button::new("ask-auto-choose-folder", "Choose project folder…")
                            .primary()
                            .on_click(cx.listener(|app, _, _, cx| {
                                app.work.ask_menu = AskMenu::Closed;
                                app.pick_workshop_folder(cx);
                                cx.notify();
                            })),
                    )
            }));
            crate::ui::Card::new("ask-mode-card-content")
                .selector(ASK_MENU_CARD_SELECTOR)
                .children([col])
                .into_any_element()
        };
        // The chip sits in the top bar, so its menu opens at the top.
        Some(
            div()
                .id("ask-mode-scrim")
                .absolute()
                .inset_0()
                .flex()
                .items_start()
                .justify_end()
                .on_click(cx.listener(|this, _ev, _w, cx| {
                    this.work.ask_menu = AskMenu::Closed;
                    cx.notify();
                }))
                .child(
                    div()
                        .id("ask-mode-card")
                        .shadow_lg()
                        .flex()
                        .flex_col()
                        .w(theme::z(320.))
                        .mt(theme::bar_h())
                        .mr(theme::z(theme::SP_12))
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(inner),
                )
                .into_any_element(),
        )
    }

    /// Apply `mode` to `key` and close the menu (U4f): the mode table takes
    /// the choice, and for the CURRENT Work conversation the shared mode cell
    /// is updated too, so a run in flight picks the new mode up at its next
    /// approval gate (U4d installs it at dispatch; this covers mid-run
    /// switches).
    pub(crate) fn set_ask_mode(&mut self, key: AskKey, mode: ApprovalMode, cx: &mut Context<Self>) {
        if self.ask_key_running(&key) {
            self.show_toast(
                "Stop the current turn before changing its consent choice",
                cx,
            );
            return;
        }
        if mode == ApprovalMode::Auto {
            if let Some(reason) = self.ask_menu_refusal(&key) {
                self.show_toast(reason, cx);
                return;
            }
        }
        self.work.ask_modes.set(key.clone(), mode);
        // For the CURRENT Work conversation, the shared mode cell is updated
        // too, so a run in flight picks the new mode up at its next approval
        // gate (U4d installs it at dispatch; this covers mid-run switches).
        if let AskKey::Work(id) = &key {
            let current = self
                .work
                .conv_id
                .clone()
                .unwrap_or_else(|| self.sidebar.active_id().to_string());
            if id == &current {
                self.work.approval_handle.set(mode);
            }
        }
        self.work.ask_menu = AskMenu::Closed;
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ask_mode::code_support;

    /// Ask applies immediately; Auto needs the one confirmation first;
    /// confirming it applies Auto to the key.
    #[test]
    fn u4f_ask_is_immediate_auto_needs_confirmation() {
        let k = AskKey::Work("u4f-conv".to_string());
        assert_eq!(
            choose(
                &AskMenu::Open(k.clone()),
                ApprovalMode::AskBeforeChanges,
                None
            ),
            MenuStep::SetMode(k.clone(), ApprovalMode::AskBeforeChanges)
        );
        assert_eq!(
            choose(&AskMenu::Open(k.clone()), ApprovalMode::Auto, None),
            MenuStep::Confirm(k.clone())
        );
        assert_eq!(
            choose(&AskMenu::ConfirmAuto(k.clone()), ApprovalMode::Auto, None),
            MenuStep::SetMode(k, ApprovalMode::Auto)
        );
        // The confirmation does not accept Ask.
        assert_eq!(
            choose(
                &AskMenu::ConfirmAuto(AskKey::Work("c".to_string())),
                ApprovalMode::AskBeforeChanges,
                None
            ),
            MenuStep::Nothing
        );
    }

    /// Auto with a refusal is refused with the toast message, and nothing
    /// else changes: no SetMode, no confirmation.
    #[test]
    fn u4f_auto_refused_for_broad_folder() {
        let k = AskKey::Work("u4f-conv".to_string());
        let msg = auto_refusal(Some("/")).expect("/ is over-broad");
        assert_eq!(
            choose(
                &AskMenu::Open(k.clone()),
                ApprovalMode::Auto,
                Some(msg.clone())
            ),
            MenuStep::Refuse(msg.clone()),
            "Auto with a refusal must refuse, not confirm or set"
        );
    }

    /// A closed menu does nothing for any pick.
    #[test]
    fn u4f_closed_menu_does_nothing() {
        for pick in [ApprovalMode::AskBeforeChanges, ApprovalMode::Auto] {
            assert_eq!(choose(&AskMenu::Closed, pick, None), MenuStep::Nothing);
        }
    }

    /// U4f removed the toolbar autonomy toggle: no `"code-autonomy"` id, no
    /// `toggle_code_autonomy`, no "Autonomous" label literal in `work.rs`.
    #[test]
    fn u4f_toolbar_has_no_autonomy_toggle() {
        let src = include_str!("work.rs");
        let toggle_id = concat!("\"code-", "autonomy\"");
        assert!(
            !src.contains(toggle_id),
            "work.rs must not keep the code-autonomy toggle switch"
        );
        let fn_name = concat!("toggle_code_", "autonomy");
        assert!(
            !src.contains(fn_name),
            "work.rs must not keep toggle_code_autonomy"
        );
        let label = concat!("\"Auto", "nomous\"");
        assert!(
            !src.contains(label),
            "work.rs must not keep the \"Autonomous\" toolbar label"
        );
    }

    // ---- U4f-FIX1 -----------------------------------------------------------

    /// Review P2: every chip — including the ones that cannot switch (fleet
    /// gateway, GLM, DeepSeek, Build) — carries its explanation, and the
    /// render attaches the tooltip outside the clickable branch.
    #[test]
    fn u4f_fix_every_chip_has_its_tooltip() {
        use crate::ask_mode::{chip_text, Effective, NoAsk};
        for eff in [
            Effective::AskPerAction,
            Effective::AskPlanFirst,
            Effective::Auto,
            Effective::NoAsk(NoAsk::FleetGateway),
            Effective::NoAsk(NoAsk::Glm),
            Effective::NoAsk(NoAsk::DeepSeek),
            Effective::NoAsk(NoAsk::Build),
        ] {
            assert!(
                !chip_text(eff).tooltip.trim().is_empty(),
                "{eff:?} has a tooltip"
            );
        }
        let src = include_str!("ask_mode_ui.rs");
        let body = &src[src.find("pub(crate) fn ask_mode_chip(").unwrap()..];
        let body = &body[..body.find("\n    }\n").unwrap()];
        let tip = body
            .find(".tooltip(text.tooltip)")
            .expect("the chip attaches its tooltip");
        let clickable = body.find(".on_click(").expect("clicks are conditional");
        assert!(
            tip < clickable,
            "the tooltip is not inside the clickable branch"
        );
        assert_eq!(body.matches(".tooltip(").count(), 1);
    }

    /// Review P2: the menu subtitles are the exact D1 copy.
    #[test]
    fn u4f_fix_menu_subtitles_are_the_d1_copy() {
        assert_eq!(
            menu_rows(),
            [
                (
                    ApprovalMode::AskBeforeChanges,
                    "Ask before changes",
                    "Stops for your OK before files change or commands run.",
                ),
                (ApprovalMode::Auto, "Auto", "Runs without asking."),
            ]
        );
        // A native harness plans first: the row says so, nothing else changes.
        let native = menu_rows_for(AskSupport::PlanFirst);
        assert_eq!(native[0].0, ApprovalMode::AskBeforeChanges);
        assert_eq!(native[0].1, "Plan first");
        assert_eq!(native[0].2, PLAN_FIRST_SUBTITLE);
        assert_eq!(native[1], menu_rows()[1]);
        assert_eq!(menu_rows_for(AskSupport::PerAction), menu_rows());
        let src = include_str!("ask_mode_ui.rs");
        assert!(
            src.contains(
                "let rows = menu_rows_for(support);
            let refusal = self.ask_menu_refusal(&key);"
            ),
            "the popover uses them"
        );
        for old in [
            concat!("The agent asks you before it ", "changes anything"),
            concat!("(the floors ", "still hold)"),
        ] {
            assert!(!src.contains(old), "the old subtitle is gone: {old}");
        }
    }

    /// Review P3: a chip whose run is in flight is dimmed and inert (not
    /// warning-coloured); otherwise warn postures are in the warning colour
    /// and only switchable postures take a click.
    #[test]
    fn u4f_fix_running_chip_is_dimmed_and_inert() {
        for (warn, switchable) in [(false, true), (true, true), (true, false), (false, false)] {
            let look = chip_look(warn, switchable, true);
            assert_eq!(look.text, theme::p().faint, "running dims the label");
            assert_eq!(look.icon, theme::p().faint, "running dims the icon");
            assert!(!look.clickable, "running is inert");
        }
        let auto = chip_look(true, true, false);
        assert_eq!(
            (auto.text, auto.icon),
            (theme::p().chip_warn, theme::p().chip_warn)
        );
        assert!(auto.clickable);
        let ask = chip_look(false, true, false);
        assert_eq!(
            (ask.text, ask.icon),
            (theme::p().chip_text, theme::p().chip_text)
        );
        assert!(ask.clickable);
        let gateway = chip_look(false, false, false);
        assert_eq!(gateway.text, theme::p().chip_text);
        assert!(
            !gateway.clickable,
            "a non-switchable chip never opens the menu"
        );
    }

    // ---- R1i ------------------------------------------------------------------

    /// R1i: the lock is only shown where changes are really held for approval.
    #[test]
    fn r1i_chip_icon_never_shows_a_lock_on_runs_without_asking() {
        use crate::ask_mode::{chip_text, Effective, NoAsk};
        for eff in [
            Effective::AskPerAction,
            Effective::AskPlanFirst,
            Effective::Auto,
            Effective::NoAsk(NoAsk::FleetGateway),
            Effective::NoAsk(NoAsk::Glm),
            Effective::NoAsk(NoAsk::DeepSeek),
            Effective::NoAsk(NoAsk::Build),
        ] {
            let text = chip_text(eff);
            let icon = chip_icon(eff);
            if text.label == "Runs without asking" || eff == Effective::Auto {
                assert_eq!(icon, Icon::Zap, "{eff:?}");
            }
            if icon == Icon::Lock {
                assert!(
                    !text.warn,
                    "{eff:?}: a lock never sits on a warning posture"
                );
            }
        }
        assert_eq!(chip_icon(Effective::AskPlanFirst), Icon::Lock);
        // The chip render uses it, and wires the explain click for the
        // postures that cannot switch.
        let src = include_str!("ask_mode_ui.rs");
        let body = &src[src.find("pub(crate) fn ask_mode_chip(").unwrap()..];
        let body = &body[..body.find("\n    }\n").unwrap()];
        assert!(body.contains("let icon = chip_icon(eff);"), "{body}");
        let dead = body
            .find("if text.switchable")
            .expect("the dead chip takes a click");
        assert!(body[dead..].contains("app.explain_ask_mode_chip(eff, cx);"));
    }

    /// R1i (C3): clicking a chip that cannot switch (GLM here) shows why, and
    /// the explanation stays; a switchable posture shows nothing.
    #[gpui::test]
    fn r1i_dead_chip_click_explains_itself(cx: &mut gpui::TestAppContext) {
        use crate::ask_mode::{chip_text, Effective, NoAsk};
        use gpui::AppContext;
        const CHILD: &str = "AWILI_R1I_TEST_HOME";
        let test = "ask_mode_ui::tests::r1i_dead_chip_click_explains_itself";
        if std::env::var_os(CHILD).is_none() {
            let home = tempfile::tempdir().unwrap();
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test, "--nocapture"])
                .env(CHILD, home.path())
                .env("HOME", home.path())
                .env("XDG_CONFIG_HOME", home.path().join("config"))
                .env("XDG_DATA_HOME", home.path().join("data"))
                .env("XDG_CACHE_HOME", home.path().join("cache"))
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                out.status.success() && stdout.contains("1 passed"),
                "isolated test failed:\n{stdout}\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        let entity = cx.new(crate::AwiliApp::new);
        entity.update(cx, |app, cx| {
            app.explain_ask_mode_chip(Effective::AskPlanFirst, cx);
            assert!(
                app.toast.msg.is_none(),
                "a switchable chip opens its menu instead"
            );
            app.explain_ask_mode_chip(Effective::NoAsk(NoAsk::Glm), cx);
            assert_eq!(
                app.toast.msg.as_ref().map(|m| m.to_string()),
                Some(chip_text(Effective::NoAsk(NoAsk::Glm)).tooltip.to_string())
            );
        });
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(10));
        cx.run_until_parked();
        entity.update(cx, |app, _cx| {
            assert!(
                app.toast.msg.is_some(),
                "the explanation stays until clicked"
            );
        });
    }

    // ---- R1i-FIX1 -------------------------------------------------------------

    /// R1i-FIX1 (review P1): every chip case by name. The lock sits exactly
    /// on the two asking postures; Auto and every `NoAsk` get the bolt — the
    /// Fleet Gateway and Build included, whose tooltips say Awili can't hold
    /// their file changes for approval.
    #[test]
    fn r1i_fix1_chip_icon_locks_only_where_awili_asks() {
        use crate::ask_mode::{Effective, NoAsk};
        assert_eq!(chip_icon(Effective::AskPerAction), Icon::Lock);
        assert_eq!(chip_icon(Effective::AskPlanFirst), Icon::Lock);
        assert_eq!(chip_icon(Effective::Auto), Icon::Zap);
        assert_eq!(
            chip_icon(Effective::NoAsk(NoAsk::FleetGateway)),
            Icon::Zap,
            "Fleet Gateway: Awili can't hold its file changes, so no lock"
        );
        assert_eq!(chip_icon(Effective::NoAsk(NoAsk::Glm)), Icon::Zap);
        assert_eq!(chip_icon(Effective::NoAsk(NoAsk::DeepSeek)), Icon::Zap);
        assert_eq!(
            chip_icon(Effective::NoAsk(NoAsk::Build)),
            Icon::Zap,
            "Build: Awili can't pause it, so no lock"
        );
    }

    /// R1i-FIX1 (review P1): the lock follows the posture source of truth —
    /// the stored mode folded with the executor's support by `effective` —
    /// for every Work executor and every Code engine, in both modes: a lock
    /// only when the mode is Ask AND the executor can be asked.
    #[test]
    fn r1i_fix1_chip_icon_follows_the_effective_posture() {
        use crate::work::WorkExecutor;
        use awili_config::EngineKind;
        let agent = |engine| WorkExecutor::Agent {
            engine,
            model: None,
        };
        let gateway = work_support(&WorkExecutor::Gateway, EngineKind::HermesGateway);
        let build = work_support(
            &WorkExecutor::LocalBuild {
                target_id: "r1i-fix1".into(),
            },
            EngineKind::Awili,
        );
        let claude = code_support(CodeEngine::Claude);
        let cases: Vec<(&str, AskSupport)> = vec![
            (
                "in-app engine",
                work_support(&WorkExecutor::Gateway, EngineKind::Awili),
            ),
            ("fleet gateway", gateway),
            ("build", build),
            (
                "claude (work)",
                work_support(&agent(CodeEngine::Claude), EngineKind::Awili),
            ),
            (
                "codex (work)",
                work_support(&agent(CodeEngine::Codex), EngineKind::Awili),
            ),
            (
                "glm (work)",
                work_support(&agent(CodeEngine::Glm), EngineKind::Awili),
            ),
            (
                "deepseek (work)",
                work_support(&agent(CodeEngine::Dsh), EngineKind::Awili),
            ),
            ("claude (code)", claude),
            ("codex (code)", code_support(CodeEngine::Codex)),
            ("glm (code)", code_support(CodeEngine::Glm)),
            ("deepseek (code)", code_support(CodeEngine::Dsh)),
        ];
        for (name, support) in cases {
            let can_ask = !matches!(support, AskSupport::Unavailable(_));
            for mode in [ApprovalMode::AskBeforeChanges, ApprovalMode::Auto] {
                let want = if can_ask && mode == ApprovalMode::AskBeforeChanges {
                    Icon::Lock
                } else {
                    Icon::Zap
                };
                assert_eq!(
                    chip_icon(effective(mode, support)),
                    want,
                    "{name} in {mode:?}"
                );
            }
        }
        let ask = ApprovalMode::AskBeforeChanges;
        assert_eq!(
            chip_icon(effective(ask, gateway)),
            Icon::Zap,
            "Fleet Gateway in Ask mode: no lock"
        );
        assert_eq!(
            chip_icon(effective(ask, build)),
            Icon::Zap,
            "Build in Ask mode: no lock"
        );
        assert_eq!(
            chip_icon(effective(ask, claude)),
            Icon::Lock,
            "an engine that can ask, in Ask mode: the lock"
        );
    }

    /// Pick `engine` as the shown Work thread's next-turn target the way the
    /// chooser does (the route, not the ambient executor, decides the chip).
    fn choose_target(
        app: &mut crate::AwiliApp,
        engine: CodeEngine,
        cx: &mut gpui::Context<crate::AwiliApp>,
    ) {
        let id = app.sidebar.active_id().to_string();
        assert!(app.set_composer_target(
            &crate::workshop_composer::ModelOwner::Work(id),
            crate::work::WorkExecutor::Agent {
                engine,
                model: None
            },
            cx,
        ));
    }

    /// R1i-FIX1: run `test` in a subprocess with a private HOME
    /// (`AwiliApp::new` reads config and opens persistence). `Some(home)` in
    /// the child (run the body); `None` in the parent once the child passed.
    fn r1i_fix1_isolated(test: &str) -> Option<std::path::PathBuf> {
        const CHILD: &str = "AWILI_R1I_FIX1_TEST_HOME";
        if let Some(home) = std::env::var_os(CHILD) {
            return Some(home.into());
        }
        let home = tempfile::tempdir().unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(CHILD, home.path())
            .env("HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path().join("config"))
            .env("XDG_DATA_HOME", home.path().join("data"))
            .env("XDG_CACHE_HOME", home.path().join("cache"))
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains(test) && stdout.contains("1 passed"),
            "isolated test failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        None
    }

    /// R1i-FIX1 (review P2): the real app is rendered in a test window with
    /// GLM as the Work executor (a chip that cannot switch). A mouse click on
    /// the rendered chip, through its own `on_click`, shows the GLM
    /// explanation and opens no menu; 10 s later the explanation is still
    /// there. A click on a switchable chip (Claude) opens the menu instead.
    #[gpui::test]
    fn r1i_fix1_rendered_dead_chip_click_explains_itself(cx: &mut gpui::TestAppContext) {
        use crate::ask_mode::{chip_text, Effective, NoAsk};
        use crate::work::Mode;
        let Some(home) = r1i_fix1_isolated(
            "ask_mode_ui::tests::r1i_fix1_rendered_dead_chip_click_explains_itself",
        ) else {
            return;
        };
        let (view, window) = cx.add_window_view(|_window, cx| crate::AwiliApp::new(cx));
        window.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(800.)));
        let key = view.update(window, |app, cx| {
            app.onboarding = crate::onboarding::Onboarding::existing();
            app.fleet_pack = None;
            app.work
                .graduation
                .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
            app.mode = Mode::Work;
            choose_target(app, CodeEngine::Glm, cx);
            app.work.agents.claude_bin = Some(home.join("no-such-claude").display().to_string());
            let key = app.work_chip_key();
            app.work
                .ask_modes
                .set(key.clone(), ApprovalMode::AskBeforeChanges);
            assert!(app.toast.visible().is_none(), "setup: no toast yet");
            cx.notify();
            key
        });
        window.run_until_parked();
        let chip = window
            .debug_bounds(ASK_MODE_CHIP_SELECTOR)
            .expect("the GLM chip is on screen");
        window.simulate_click(chip.center(), gpui::Modifiers::none());
        window.run_until_parked();
        let why = chip_text(Effective::NoAsk(NoAsk::Glm)).tooltip;
        view.update(window, |app, _cx| {
            assert_eq!(
                app.toast.visible().map(|m| m.to_string()),
                Some(why.to_string()),
                "the click on the rendered chip shows why it cannot switch"
            );
            assert_eq!(
                app.work.ask_menu,
                AskMenu::Closed,
                "a chip that cannot switch opens no menu"
            );
        });
        window
            .executor()
            .advance_clock(std::time::Duration::from_secs(10));
        window.run_until_parked();
        view.update(window, |app, _cx| {
            assert_eq!(
                app.toast.visible().map(|m| m.to_string()),
                Some(why.to_string()),
                "the explanation stays until clicked"
            );
        });
        // A switchable chip (Claude) still opens its menu, and explains nothing.
        view.update(window, |app, cx| {
            app.dismiss_toast(cx);
            choose_target(app, CodeEngine::Claude, cx);
            cx.notify();
        });
        window.run_until_parked();
        let chip = window
            .debug_bounds(ASK_MODE_CHIP_SELECTOR)
            .expect("the Claude chip is on screen");
        window.simulate_click(chip.center(), gpui::Modifiers::none());
        window.run_until_parked();
        view.update(window, |app, _cx| {
            assert_eq!(app.work.ask_menu, AskMenu::Open(key.clone()));
            assert!(
                app.toast.visible().is_none(),
                "a switchable chip shows no toast"
            );
        });
    }

    /// The rendered Ask/Auto card keeps its consent boundary through the real
    /// KIT row and button click paths: Ask is immediate, Auto requires the
    /// explicit confirm, Keep and outside dismissal preserve Ask, and an
    /// over-broad folder refuses Auto with the existing explanation.
    #[gpui::test]
    fn rendered_ask_mode_popover_keeps_consent_boundaries(cx: &mut gpui::TestAppContext) {
        use crate::work::Mode;

        let Some(home) = r1i_fix1_isolated(
            "ask_mode_ui::tests::rendered_ask_mode_popover_keeps_consent_boundaries",
        ) else {
            return;
        };
        let folder = home.join("project");
        std::fs::create_dir_all(&folder).unwrap();
        cx.executor().allow_parking();
        let (view, window) = cx.add_window_view(|_window, cx| crate::AwiliApp::new(cx));
        window.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(800.)));

        let key = view.update(window, |app, cx| {
            app.onboarding = crate::onboarding::Onboarding::existing();
            app.fleet_pack = None;
            app.work
                .graduation
                .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
            app.mode = Mode::Work;
            choose_target(app, CodeEngine::Claude, cx);
            app.work.agents.claude_bin = Some(folder.join("no-such-claude").display().to_string());
            app.work.folder = Some(folder.clone());
            let key = app.work_chip_key();
            app.work.ask_modes.set(key.clone(), ApprovalMode::Auto);
            app.work.approval_handle.set(ApprovalMode::Auto);
            app.work.ask_menu = AskMenu::Open(key.clone());
            cx.notify();
            key
        });
        window.run_until_parked();
        let ask_subtitle = window
            .debug_bounds("ui-text-ask-mode-row-0-subtitle")
            .expect("the Ask subtitle is rendered as KIT Text");
        let ask_row = window
            .debug_bounds(ASK_MODE_ASK_ROW_SELECTOR)
            .expect("the Ask KIT Row is rendered");
        let auto_subtitle = window
            .debug_bounds("ui-text-ask-mode-row-1-subtitle")
            .expect("the short Auto subtitle is rendered as KIT Text");
        assert!(
            auto_subtitle.size.height <= theme::z(theme::type_::LABEL.line * 1.5),
            "the Auto subtitle provides a one-line same-font baseline: {auto_subtitle:?}"
        );
        assert!(
            ask_subtitle.size.height
                > auto_subtitle.size.height + theme::z(theme::LABEL_SIZE * 0.5),
            "the Ask consent explanation wraps beyond one line: ask={ask_subtitle:?}, auto={auto_subtitle:?}"
        );
        assert!(
            ask_subtitle.origin.x + ask_subtitle.size.width <= ask_row.origin.x + ask_row.size.width,
            "the wrapped explanation stays within its actual KIT Row bounds: text={ask_subtitle:?}, row={ask_row:?}"
        );

        // Ask applies from the real rendered menu row without a second step.
        let ask = window
            .debug_bounds(ASK_MODE_ASK_ROW_SELECTOR)
            .expect("the Ask KIT Row is rendered");
        window.simulate_click(ask.center(), gpui::Modifiers::none());
        window.run_until_parked();
        view.read_with(window, |app, _| {
            assert_eq!(app.work.ask_menu, AskMenu::Closed);
            assert_eq!(
                app.work.ask_modes.mode_for(&key),
                ApprovalMode::AskBeforeChanges,
                "Ask applies immediately to the rendered menu's owner"
            );
            assert_eq!(
                app.work.approval_handle.get(),
                ApprovalMode::AskBeforeChanges
            );
        });

        // Auto first moves to the confirmation card; Keep leaves Ask intact.
        view.update(window, |app, cx| {
            app.work.ask_menu = AskMenu::Open(key.clone());
            cx.notify();
        });
        window.run_until_parked();
        let auto = window
            .debug_bounds(ASK_MODE_AUTO_ROW_SELECTOR)
            .expect("the Auto KIT Row is rendered");
        window.simulate_click(auto.center(), gpui::Modifiers::none());
        window.run_until_parked();
        view.read_with(window, |app, _| {
            assert_eq!(app.work.ask_menu, AskMenu::ConfirmAuto(key.clone()));
            assert_eq!(
                app.work.ask_modes.mode_for(&key),
                ApprovalMode::AskBeforeChanges,
                "choosing Auto only opens confirmation"
            );
            assert_eq!(
                app.work.approval_handle.get(),
                ApprovalMode::AskBeforeChanges
            );
        });
        assert!(
            window.debug_bounds(ASK_MENU_CARD_SELECTOR).is_some(),
            "the confirmation remains in the measured KIT Card"
        );
        let keep = window
            .debug_bounds("ask-auto-keep")
            .expect("the Keep asking KIT Button is rendered");
        window.simulate_click(keep.center(), gpui::Modifiers::none());
        window.run_until_parked();
        view.read_with(window, |app, _| {
            assert_eq!(app.work.ask_menu, AskMenu::Closed);
            assert_eq!(
                app.work.ask_modes.mode_for(&key),
                ApprovalMode::AskBeforeChanges,
                "Keep closes the card without changing consent"
            );
            assert_eq!(
                app.work.approval_handle.get(),
                ApprovalMode::AskBeforeChanges
            );
        });

        // The primary confirmation is the only rendered action that applies Auto.
        view.update(window, |app, cx| {
            app.work.ask_menu = AskMenu::Open(key.clone());
            cx.notify();
        });
        window.run_until_parked();
        let auto = window
            .debug_bounds(ASK_MODE_AUTO_ROW_SELECTOR)
            .expect("the Auto KIT Row is rendered again");
        window.simulate_click(auto.center(), gpui::Modifiers::none());
        window.run_until_parked();
        let confirm = window
            .debug_bounds("ask-auto-confirm")
            .expect("the Switch to Auto KIT Button is rendered");
        window.simulate_click(confirm.center(), gpui::Modifiers::none());
        window.run_until_parked();
        view.read_with(window, |app, _| {
            assert_eq!(app.work.ask_menu, AskMenu::Closed);
            assert_eq!(
                app.work.ask_modes.mode_for(&key),
                ApprovalMode::Auto,
                "the explicit confirmation applies Auto"
            );
            assert_eq!(app.work.approval_handle.get(), ApprovalMode::Auto);
        });

        // The broad-folder refusal keeps its recovery action visible and keeps Ask.
        view.update(window, |app, cx| {
            app.work.folder = Some(std::path::PathBuf::from("/"));
            app.work
                .ask_modes
                .set(key.clone(), ApprovalMode::AskBeforeChanges);
            app.work.approval_handle.set(ApprovalMode::AskBeforeChanges);
            app.work.ask_menu = AskMenu::Open(key.clone());
            cx.notify();
        });
        window.run_until_parked();
        let auto = window
            .debug_bounds(ASK_MODE_AUTO_ROW_SELECTOR)
            .expect("the Auto KIT Row is rendered for the refusal case");
        window.simulate_click(auto.center(), gpui::Modifiers::none());
        window.run_until_parked();
        let refusal = auto_refusal(Some("/")).expect("root is an over-broad folder");
        view.read_with(window, |app, _| {
            assert_eq!(app.work.ask_menu, AskMenu::Open(key.clone()));
            assert_eq!(
                app.work.ask_modes.mode_for(&key),
                ApprovalMode::AskBeforeChanges,
                "a refused Auto choice does not change consent"
            );
            assert_eq!(
                app.work.approval_handle.get(),
                ApprovalMode::AskBeforeChanges
            );
            assert_eq!(
                app.toast.visible().map(|message| message.to_string()),
                Some(refusal),
                "the refusal remains visible to the owner"
            );
        });

        assert!(
            window
                .debug_bounds("ui-button-ask-auto-choose-folder")
                .is_some(),
            "the refused Auto choice leaves a visible folder chooser"
        );

        // An outside click dismisses the real full-window scrim without a choice.
        view.update(window, |app, cx| {
            app.work.folder = Some(folder.clone());
            app.work.ask_modes.set(key.clone(), ApprovalMode::Auto);
            app.work.approval_handle.set(ApprovalMode::Auto);
            app.work.ask_menu = AskMenu::Open(key.clone());
            cx.notify();
        });
        window.run_until_parked();
        assert!(
            window.debug_bounds(ASK_MENU_CARD_SELECTOR).is_some(),
            "the menu is open before the outside click"
        );
        window.simulate_click(
            gpui::point(gpui::px(300.), gpui::px(500.)),
            gpui::Modifiers::none(),
        );
        window.run_until_parked();
        view.read_with(window, |app, _| {
            assert_eq!(app.work.ask_menu, AskMenu::Closed);
            assert_eq!(
                app.work.ask_modes.mode_for(&key),
                ApprovalMode::Auto,
                "dismissing the scrim does not change consent"
            );
            assert_eq!(app.work.approval_handle.get(), ApprovalMode::Auto);
        });
    }

    /// Selecting the next target before sending changes what the consent chip
    /// and its menu promise, from one source and together. The stored Ask
    /// choice, a running turn and its identity are untouched, and a target
    /// that is not available promises nothing.
    #[gpui::test]
    fn next_target_selection_drives_chip_and_menu_together_before_send(
        cx: &mut gpui::TestAppContext,
    ) {
        use crate::work::{Mode, WorkExecutor};
        use crate::workshop_composer::ModelOwner;
        let Some(home) = r1i_fix1_isolated(
            "ask_mode_ui::tests::next_target_selection_drives_chip_and_menu_together_before_send",
        ) else {
            return;
        };
        let (view, window) = cx.add_window_view(|_window, cx| crate::AwiliApp::new(cx));
        window.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(800.)));
        let (key, owner) = view.update(window, |app, cx| {
            app.onboarding = crate::onboarding::Onboarding::existing();
            app.fleet_pack = None;
            app.work
                .graduation
                .set_ledger(home.join("ledger.jsonl").to_str().unwrap());
            app.mode = Mode::Work;
            let key = app.work_chip_key();
            app.work
                .ask_modes
                .set(key.clone(), ApprovalMode::AskBeforeChanges);
            let AskKey::Work(id) = key.clone() else {
                unreachable!("the Work chip is keyed by its conversation")
            };
            // A turn is running under the in-app engine.
            app.work.conv_id = Some(id.clone());
            app.work.running = true;
            app.work.panel.requested_identity =
                Some(crate::work_run_identity::WorkRunIdentity::OwnedEngine {
                    model: "m".into(),
                    engine_id: None,
                });
            cx.notify();
            (key, ModelOwner::Work(id))
        });
        let agent = |engine| WorkExecutor::Agent {
            engine,
            model: None,
        };
        // (next target, chip label, the menu offers it, with this asking row)
        for (target, chip, menu) in [
            (
                WorkExecutor::InApp,
                "Ask before changes",
                Some("Ask before changes"),
            ),
            (agent(CodeEngine::Claude), "Plan first", Some("Plan first")),
            (agent(CodeEngine::Codex), "Plan first", Some("Plan first")),
            (agent(CodeEngine::Glm), "Runs without asking", None),
            (
                WorkExecutor::InApp,
                "Ask before changes",
                Some("Ask before changes"),
            ),
        ] {
            view.update(window, |app, cx| {
                assert!(app.set_composer_target(&owner, target.clone(), cx));
            });
            window.run_until_parked();
            assert!(
                window.debug_bounds(ASK_MODE_CHIP_SELECTOR).is_some(),
                "the chip is on screen for {target:?}"
            );
            view.update(window, |app, cx| {
                let next = app.next_ask(&key).expect("the target is available");
                assert_eq!(next.chip_label, chip, "{target:?}");
                app.work.ask_menu = AskMenu::Open(key.clone());
                assert_eq!(
                    app.ask_mode_popover(cx).is_some(),
                    menu.is_some(),
                    "{target:?}"
                );
                if let Some(row) = menu {
                    assert_eq!(next.menu_label, row, "{target:?}");
                    assert_eq!(next.chip_label, row, "chip and menu agree for {target:?}");
                }
                app.work.ask_menu = AskMenu::Closed;
                // Nothing about the stored choice, the running turn or its
                // identity moved with the selection.
                assert_eq!(
                    app.work.ask_modes.mode_for(&key),
                    ApprovalMode::AskBeforeChanges
                );
                assert!(app.work.running);
                assert!(matches!(
                    app.work.panel.requested_identity,
                    Some(crate::work_run_identity::WorkRunIdentity::OwnedEngine { .. })
                ));
            });
        }
        // No usable target: no promise, and no menu, from an ambient model.
        view.update(window, |app, cx| {
            let gone = AskKey::CodeTab(u64::MAX);
            assert!(app.next_ask(&gone).is_none());
            app.work.ask_menu = AskMenu::Open(gone);
            assert!(app.ask_mode_popover(cx).is_none());
            app.work.ask_menu = AskMenu::Closed;
            app.work.running = false;
            app.work.panel.requested_identity = None;
        });
    }
}
