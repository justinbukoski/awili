//! One persisted preference and the window's current native appearance.
use crate::{theme, AwiliApp};
use awili_config::{ColorMode, Config};
use gpui::{Context, Subscription, Window, WindowAppearance};

pub(crate) struct Appearance {
    pub(crate) preference: ColorMode,
    observer: Option<Subscription>,
}

impl Appearance {
    pub(crate) fn new(preference: ColorMode) -> Self {
        // System is resolved from the actual window before the first paint.
        theme::set_light_mode(preference == ColorMode::Light);
        Self {
            preference,
            observer: None,
        }
    }
}

pub(crate) fn effective_light(preference: ColorMode, native: WindowAppearance) -> bool {
    match preference {
        ColorMode::Light => true,
        ColorMode::Dark => false,
        ColorMode::System => matches!(
            native,
            WindowAppearance::Light | WindowAppearance::VibrantLight
        ),
    }
}

impl AwiliApp {
    pub(crate) fn initialize_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.appearance.observer.is_none() {
            let app = cx.entity().downgrade();
            self.appearance.observer = Some(window.observe_window_appearance(move |window, cx| {
                let _ = app.update(cx, |app, cx| app.reconcile_appearance(window, cx));
            }));
        }
        self.reconcile_appearance(window, cx);
    }

    pub(crate) fn reconcile_appearance(&mut self, window: &Window, cx: &mut Context<Self>) {
        if theme::set_light_mode(effective_light(
            self.appearance.preference,
            window.appearance(),
        )) {
            cx.notify();
        }
    }

    /// Appearance writes use the catalog's validated atomic save and undo path.
    /// Applying them changes no provider, run, Stop generation, draft or owner.
    pub(crate) fn sync_saved_appearance(&mut self, cx: &mut Context<Self>) {
        if let Some(doc) = &self.settings.doc {
            self.appearance.preference = doc.config().appearance.color_mode;
            cx.notify();
        }
    }

    pub(crate) fn saved_config(&self) -> Option<Config> {
        self.settings.doc.as_ref().map(|doc| doc.config())
    }

    pub(crate) fn appearance_only_change(before: Option<Config>, after: Option<Config>) -> bool {
        match (before, after) {
            (Some(before), Some(mut after)) => {
                after.appearance = before.appearance.clone();
                before == after
            }
            _ => false,
        }
    }

    pub(crate) fn choose_color_mode(
        &mut self,
        mode: ColorMode,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        // A closed Settings document may be stale; load the current file without
        // opening its UI or replacing a visible form's unfinished edits.
        if !self.settings.open {
            self.settings.open();
            self.settings.open = false;
        }
        self.apply_settings_item(
            "appearance.color_mode",
            crate::settings_catalog::Edit::Text(mode.as_str().into()),
            cx,
        );
        if self.settings.error.is_some() {
            self.show_info_toast(
                "Appearance was not saved. Open Settings to review the error.",
                cx,
            );
        } else {
            self.reconcile_appearance(window, cx);
        }
    }
}
