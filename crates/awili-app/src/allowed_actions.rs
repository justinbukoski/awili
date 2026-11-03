//! S6: the Allowed actions page in Settings. Every approval Awili remembers
//! (Always, Folder and this-conversation grants) in plain words: what it
//! allows, where it applies, and when it was given - with Revoke. A revoked
//! action asks again the next time (`awili_engine_log::remembered`).
//!
//! Listing is live, never copied into the settings file: the page reads the
//! per-user grant store when it opens and after every Revoke.

use awili_engine_log::grant_service::GrantStoreService;
use awili_engine_log::remembered::{self, GrantScopeKind, RememberedGrant};
use gpui::{div, prelude::*, Context, SharedString};

use crate::theme;
use crate::ui::text;
use crate::AwiliApp;

pub(crate) const EMPTY: &str = "Nothing is allowed without asking yet.";
pub(crate) const UNREADABLE: &str =
    "Awili could not read the list of allowed actions. Close Settings and open it again.";
pub(crate) const REVOKE_FAILED: &str = "Awili could not take that back. Try again.";
pub(crate) const REVOKED: &str = "Done. Awili will ask before doing that again.";

/// The page's state: the grants as last read, and a plain error when the
/// read or a Revoke failed.
#[derive(Default)]
pub(crate) struct AllowedActions {
    pub(crate) grants: Vec<RememberedGrant>,
    pub(crate) error: Option<&'static str>,
    /// Tests point this at a private store; production resolves the fixed
    /// per-user one.
    pub(crate) store_override: Option<&'static GrantStoreService>,
}

/// One grant in plain words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AllowedText {
    /// What it allows ("Write a file").
    pub(crate) what: String,
    /// What it is limited to ("Only for notes.txt"), or that it is not.
    pub(crate) limit: String,
    /// Where it applies.
    pub(crate) scope: String,
    /// When it was granted.
    pub(crate) when: String,
}

impl AllowedText {
    /// The help lines under the title, in reading order.
    pub(crate) fn lines(&self) -> [String; 2] {
        [
            self.limit.clone(),
            format!("{} \u{00b7} {}", self.scope, self.when),
        ]
    }
}

fn list_of(items: &[String]) -> String {
    items.join(", ")
}

/// The plain description of one grant's argument constraints.
fn limit_text(g: &RememberedGrant) -> String {
    let strings = |v: &serde_json::Value| -> Vec<String> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    };
    let paths = g
        .constraints
        .get("__paths")
        .map(strings)
        .unwrap_or_default();
    let commands = g
        .constraints
        .get("__commands")
        .map(strings)
        .unwrap_or_default();
    let mut parts = Vec::new();
    if !paths.is_empty() {
        parts.push(format!(
            "Only for {} {}",
            if paths.len() == 1 {
                "this path:"
            } else {
                "these paths:"
            },
            list_of(&paths)
        ));
    } else {
        // The top-level path keys carry the same values when `__paths` is
        // absent; any other key is shown as it is.
        let mut named: Vec<String> = g
            .constraints
            .iter()
            .filter(|(k, _)| !k.starts_with("__"))
            .filter_map(|(_, v)| v.as_str().map(str::to_string))
            .collect();
        named.sort();
        if !named.is_empty() {
            parts.push(format!("Only for {}", list_of(&named)));
        }
    }
    if !commands.is_empty() {
        parts.push(format!(
            "Only {} {}",
            if commands.len() == 1 {
                "this command:"
            } else {
                "these commands:"
            },
            list_of(&commands)
        ));
    }
    if parts.is_empty() {
        "Any use of it, with any details".to_string()
    } else {
        parts.join(". ")
    }
}

/// When a grant was given, in the owner's local time.
pub(crate) fn when_text(granted_at_ms: Option<u64>) -> String {
    use chrono::{Datelike, Local, TimeZone};
    let Some(ms) = granted_at_ms else {
        return "Allowed earlier (the time was not kept)".to_string();
    };
    let Some(t) = i64::try_from(ms)
        .ok()
        .and_then(|ms| Local.timestamp_millis_opt(ms).single())
    else {
        return "Allowed earlier".to_string();
    };
    let this_year = Local::now().year() == t.year();
    let day = if this_year {
        t.format("%b %-d")
    } else {
        t.format("%b %-d, %Y")
    };
    format!("Allowed {day} at {}", t.format("%-I:%M %p"))
}

pub(crate) fn describe(g: &RememberedGrant) -> AllowedText {
    let what = crate::work::plain_tool(&g.tool).map_or_else(|| g.tool.clone(), |p| p.title.into());
    let scope = match g.scope {
        GrantScopeKind::Session => "This conversation only".to_string(),
        GrantScopeKind::Folder => format!(
            "In the folder {}",
            g.folder_root.as_deref().unwrap_or("it was given for")
        ),
        GrantScopeKind::Always => "In all projects".to_string(),
    };
    AllowedText {
        what,
        limit: limit_text(g),
        scope,
        when: when_text(g.granted_at_ms),
    }
}

impl AwiliApp {
    /// The store the page reads, if one exists yet. A missing store is not an
    /// error (nothing was ever remembered): only this conversation's
    /// grants can be listed then.
    fn allowed_store(&self) -> Result<Option<&'static GrantStoreService>, ()> {
        if let Some(s) = self.allowed_actions.store_override {
            return Ok(Some(s));
        }
        let Some(root) = crate::work::engine_data_root() else {
            return Ok(None);
        };
        let path = root.join("grants.log");
        if !path.exists() {
            return Ok(None);
        }
        GrantStoreService::init(path, None)
            .map(Some)
            .map_err(|_| ())
    }

    /// Read the list again.
    pub(crate) fn reload_allowed_actions(&mut self) {
        let read = match self.allowed_store() {
            Ok(Some(svc)) => svc.list_remembered().map_err(|_| ()),
            Ok(None) => Ok(remembered::list_session_only()),
            Err(()) => Err(()),
        };
        match read {
            Ok(grants) => {
                self.allowed_actions.grants = grants;
                self.allowed_actions.error = None;
            }
            Err(()) => {
                self.allowed_actions.grants.clear();
                self.allowed_actions.error = Some(UNREADABLE);
            }
        }
    }

    /// Revoke the grant on row `ix`, then read the list again.
    pub(crate) fn revoke_allowed_action(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(g) = self.allowed_actions.grants.get(ix).cloned() else {
            return;
        };
        let done = match self.allowed_store() {
            Ok(Some(svc)) => svc.revoke_grant(&g).is_ok(),
            Ok(None) => {
                for r in &g.all_refs {
                    remembered::revoke_session_grant(r);
                }
                true
            }
            Err(()) => false,
        };
        self.reload_allowed_actions();
        if done {
            self.show_info_toast(REVOKED, cx);
        } else {
            self.allowed_actions.error = Some(REVOKE_FAILED);
        }
        cx.notify();
    }

    /// The page's rows under the blurb.
    pub(crate) fn allowed_actions_rows(&self, cx: &mut Context<Self>) -> Vec<gpui::AnyElement> {
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        if let Some(e) = self.allowed_actions.error {
            rows.push(
                div()
                    .debug_selector(|| "allowed-error".to_string())
                    .px(theme::z(theme::SP_16))
                    .pt(theme::z(theme::SP_12))
                    .child(
                        text::ui_text(e)
                            .text_color_override(theme::p().chip_warn)
                            .wrap(),
                    )
                    .into_any_element(),
            );
        }
        if self.allowed_actions.grants.is_empty() && self.allowed_actions.error.is_none() {
            rows.push(
                div()
                    .debug_selector(|| "allowed-empty".to_string())
                    .p(theme::z(theme::SP_16))
                    .child(text::ui_text(EMPTY).reading())
                    .into_any_element(),
            );
        }
        for (ix, g) in self.allowed_actions.grants.iter().enumerate() {
            let t = describe(g);
            let revoke = self.sx_button(("sx-revoke", ix), "Revoke", false, cx, move |t, cx| {
                t.revoke_allowed_action(ix, cx)
            });
            let [limit, where_when] = t.lines();
            let row =
                crate::ui::SettingRow::new(("allowed-row-ui", ix), SharedString::from(t.what))
                    .help(SharedString::from(format!("{limit}\n{where_when}")))
                    .control(move |_reason| revoke);
            rows.push(
                div()
                    .debug_selector(move || format!("allowed-row-{ix}"))
                    .mx(theme::z(theme::SP_12))
                    .child(row)
                    .into_any_element(),
            );
        }
        rows
    }
}

#[cfg(test)]
#[path = "allowed_actions_tests.rs"]
mod tests;
