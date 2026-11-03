//! RC2-R58 (usability RC2 round 4, Major 5: the Skills, Automations and
//! System pages were dead ends in jargon that contradicted each other —
//! "dashboard unreachable at http://127.0.0.1:9119: error sending request
//! for url (…)", "Hermes not configured — set [hermes] in config.toml.",
//! "Fleet gateway unreachable … the Hermes service, which is not set up
//! yet").
//!
//! The four pages that come from Hermes (the owner's fleet gateway: Skills,
//! Automations, System, Messaging) now say, in the same words: what the page
//! is, what state Hermes is in (one sentence, the Work Connection card's
//! own gateway line), and what the owner can do — with "Check again" and
//! "Open Settings" buttons right there.

use gpui::{div, prelude::*, Context, SharedString};

use crate::theme;
use crate::AwiliApp;

/// The pages that show what Hermes has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HermesPage {
    Skills,
    Automations,
    System,
    Messaging,
}

impl HermesPage {
    /// What the page is, in one sentence.
    pub(crate) fn intro(self) -> &'static str {
        match self {
            Self::Skills => {
                "Skills are the ready-made abilities Hermes, your fleet gateway, can use."
            }
            Self::Automations => {
                "Automations are the jobs Hermes, your fleet gateway, runs on a schedule."
            }
            Self::System => {
                "System shows how Hermes, your fleet gateway, and its computer are doing."
            }
            Self::Messaging => "Messaging connects Hermes, your fleet gateway, to your chat apps.",
        }
    }

    /// RC2-R65 (usability RC2 round 5, Minor 8: System and Messaging had no
    /// title; the text started at the top left): the page's title, shown
    /// above the notice on the pages that have no heading of their own.
    pub(crate) fn title(self) -> Option<&'static str> {
        match self {
            Self::System => Some("System"),
            Self::Messaging => Some("Messaging"),
            Self::Skills | Self::Automations => None,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Skills => "skills",
            Self::Automations => "automations",
            Self::System => "system",
            Self::Messaging => "messaging",
        }
    }
}

/// The test lookup key of a page's notice.
pub(crate) fn notice_selector(page: HermesPage) -> String {
    format!("rc2r58-hermes-notice-{}", page.key())
}

/// RC2-R65: the test lookup key of a page's title over its notice.
pub(crate) fn title_selector(page: HermesPage) -> String {
    format!("rc2r65-hermes-title-{}", page.key())
}

/// RC2-R65: the test lookup key of a page's "Checked again at …" line.
pub(crate) fn checked_selector(page: HermesPage) -> String {
    format!("rc2r65-hermes-checked-{}", page.key())
}

/// RC2-R65 (usability RC2 round 5, Minor 8: "Check again" showed no
/// feedback): the line a page shows once its Check again was pressed — the
/// local time, so a second press visibly changes it.
pub(crate) fn checked_line(at: &str) -> String {
    format!("Checked again at {at}.")
}

/// RC2-R65: the local time of a Check again, as the line shows it.
pub(crate) fn check_time() -> String {
    chrono::Local::now().format("%H:%M:%S").to_string()
}

/// A failure to read Hermes, in plain words: no raw client error
/// ("error sending request for url (…)").
pub(crate) fn plain_read_failure(url: &str, err: &str) -> String {
    let l = err.to_ascii_lowercase();
    let reason = if [
        "error sending request",
        "connection refused",
        "unreachable",
        "timed out",
        "dns error",
    ]
    .iter()
    .any(|w| l.contains(w))
    {
        "nothing answers there".to_string()
    } else {
        let first: String = err.lines().next().unwrap_or("").chars().take(120).collect();
        first
    };
    format!("Awili couldn\u{2019}t read Hermes at {url}: {reason}.")
}

impl AwiliApp {
    /// The notice a Hermes page shows instead of its content, or `None`
    /// when Hermes answers and the page can show what it has. Every page
    /// builds it the same way: its own sentence, then the same state
    /// sentence ([`Self::command_center_unavailable_text`]), then the
    /// address the owner can change.
    ///
    /// RC2-R5M2 (slice review befb867d, P2): built from each page's own
    /// read state. Before, Automations, System and Messaging returned no
    /// notice whenever Hermes was turned on, so a failed read lost the
    /// state sentence, "Check again" and "Open Settings".
    pub(crate) fn hermes_page_notice(&self, page: HermesPage) -> Option<String> {
        let hermes = self.work.hermes.enabled;
        let gateway = self.work.gateway.enabled;
        match self.hermes_page_read_error(page) {
            // Read through Hermes's own address: say why, plainly.
            Some(err) if hermes => {
                let url = if gateway {
                    self.work.gateway.dashboard_url.as_str()
                } else {
                    self.work.hermes.base_url.as_str()
                };
                return Some(format!("{} {}", page.intro(), plain_read_failure(url, err)));
            }
            Some(_) => {}
            // Hermes answered (or is being read): the page shows what it has.
            None if hermes => return None,
            // Skills are also read from the gateway's dashboard alone.
            None if gateway && page == HermesPage::Skills => return None,
            None => {}
        }
        let mut text = format!(
            "{} {}",
            page.intro(),
            self.command_center_unavailable_text()
        );
        if gateway {
            // RC2-R65 (usability RC2 round 5, Minor 8: "… which is not set
            // yet … Its address … is http://127.0.0.1:9119" read as the same
            // address twice): the gateway's address is named as the
            // gateway's, apart from the service address that is missing.
            text.push_str(&format!(
                " The fleet gateway itself is at {} (Settings > Advanced > Hermes gateway \
                 address).",
                self.work.gateway.dashboard_url
            ));
        }
        Some(text)
    }

    /// RC2-R65: the page is being read (or the gateway checked) right now.
    pub(crate) fn hermes_page_loading(&self, page: HermesPage) -> bool {
        self.work.health.gateway.probing()
            || match page {
                HermesPage::Skills => self.skills_state.loading,
                HermesPage::Automations => self.cron_state.loading,
                HermesPage::System => self.command_center_data.loading,
                HermesPage::Messaging => self.messaging_data.loading,
            }
    }

    /// RC2-R5M2: the error of the page's own last read.
    ///
    /// RC2-R5M6 (slice review 3775ed08, P2): also when the page keeps what
    /// it read before (System a status, Messaging a snapshot): the page
    /// shows that with the notice above it, never stale content alone.
    fn hermes_page_read_error(&self, page: HermesPage) -> Option<&str> {
        match page {
            HermesPage::Skills => self.skills_state.error.as_deref(),
            HermesPage::Automations => self.cron_state.error.as_deref(),
            HermesPage::System => self.command_center_data.error.as_deref(),
            HermesPage::Messaging => self.messaging_data.error.as_deref(),
        }
    }

    /// The notice element: the words, then "Check again" and "Open
    /// Settings".
    pub(crate) fn hermes_page_notice_element(
        &self,
        page: HermesPage,
        text: String,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        // RC2-R65: while the page is read again it says so.
        let checking = self.hermes_page_loading(page);
        let check = crate::work::pill_button(
            "rc2r58-check-again",
            if checking {
                "Checking\u{2026}"
            } else {
                "Check again"
            },
            theme::p().chip_border,
            theme::p().chip_text,
            false,
        )
        .on_click(cx.listener(move |this, _ev, _w, cx| this.hermes_check_again(page, cx)));
        let settings = crate::work::pill_button(
            "rc2r58-open-settings",
            "Open Settings",
            theme::p().chip_border,
            theme::p().chip_text,
            false,
        )
        .on_click(cx.listener(|this, _ev, _w, cx| {
            this.open_settings(cx);
            this.select_settings_page(crate::settings_catalog::Page::Advanced, cx);
            cx.notify();
        }));
        let checked = self.checked_at.get(page.key()).map(|at| {
            div()
                .debug_selector(move || checked_selector(page))
                .text_size(theme::z(theme::LABEL_SIZE))
                .text_color(theme::p().faint)
                .child(SharedString::from(checked_line(at)))
        });
        div()
            .debug_selector(move || notice_selector(page))
            .p(theme::z(16.))
            .flex()
            .flex_col()
            .gap(theme::z(10.))
            .children(page.title().map(|title| {
                div()
                    .debug_selector(move || title_selector(page))
                    .text_size(theme::z(theme::heading_size(1)))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::p().text)
                    .child(title)
            }))
            .child(
                div()
                    .text_size(theme::z(theme::TEXT_SIZE))
                    .text_color(theme::p().muted)
                    .child(SharedString::from(text)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(theme::z(8.))
                    .child(check)
                    .child(settings),
            )
            .children(checked)
            .into_any_element()
    }

    /// "Check again": ask the gateway anew and reload the page.
    pub(crate) fn hermes_check_again(&mut self, page: HermesPage, cx: &mut Context<Self>) {
        self.checked_at.insert(page.key().to_string(), check_time());
        if self.work.gateway.enabled {
            self.start_gateway_probe(cx);
        }
        match page {
            HermesPage::Skills => self.refresh_skills(cx),
            HermesPage::Automations => self.refresh_cron(cx),
            HermesPage::System => self.refresh_command_center(cx),
            HermesPage::Messaging => self.refresh_messaging(cx),
        }
        cx.notify();
    }
}

#[cfg(test)]
#[path = "hermes_pages_tests.rs"]
mod tests;
