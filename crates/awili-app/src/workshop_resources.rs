//! Folder and session resources in the Workshop title menu (PLAN.md §8).
use std::path::PathBuf;

use gpui::{Context, Window};

use crate::{
    icons::Icon,
    sessions::SessionsView,
    ui,
    work::{relative_time, Mode},
    workshop::WorkshopMenu,
    workspace::FileTreeState,
    AwiliApp,
};

#[cfg(test)]
#[path = "workshop_resource_tests.rs"]
mod tests;

/// A reader separate from the runner's workspace. Paths and outputs are
/// captured when opened, so navigating never retargets an unsettled job.
#[derive(Default)]
pub(crate) struct WorkshopResources {
    owner: Option<String>,
    root: Option<PathBuf>,
    directory: Option<PathBuf>,
    tree: Option<FileTreeState>,
    error: Option<String>,
    outputs: Vec<(String, PathBuf)>,
}

impl AwiliApp {
    pub(crate) fn workshop_resource_actions(
        &self,
        mut menu: ui::Menu,
        cx: &mut Context<Self>,
    ) -> ui::Menu {
        menu = menu.separator();
        for (label, kind) in [
            ("Browse files…", WorkshopMenu::Files),
            ("Outputs…", WorkshopMenu::Outputs),
            ("Recent sessions…", WorkshopMenu::RecentSessions),
        ] {
            let mut item =
                ui::MenuItem::new(label).on_click(cx.listener(move |app, _, window, cx| {
                    app.open_workshop_resources(kind, window, cx)
                }));
            if kind != WorkshopMenu::RecentSessions && self.workshop_folder().is_none() {
                item = item.disabled("Choose a folder for this thread first.");
            }
            menu = menu.item(item);
        }
        menu
    }

    pub(crate) fn open_workshop_resources(
        &mut self,
        kind: WorkshopMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if kind == WorkshopMenu::RecentSessions {
            if self.work.hermes_sessions.is_empty() {
                self.refresh_sessions(cx);
            }
        } else {
            let Some(folder) = self.workshop_folder().map(PathBuf::from) else {
                return;
            };
            let owned_outputs = self.work.panel_is_visible(self.sidebar.active_id())
                && self.work.folder.as_ref() == Some(&folder);
            self.shell.tabs.resources = WorkshopResources {
                owner: Some(self.sidebar.active_id().to_owned()),
                root: Some(folder.clone()),
                directory: Some(folder.clone()),
                tree: None,
                error: None,
                outputs: if owned_outputs {
                    self.work
                        .outputs
                        .iter()
                        .map(|name| (name.clone(), folder.join(name)))
                        .collect()
                } else {
                    Vec::new()
                },
            };
            if kind == WorkshopMenu::Files {
                self.read_workshop_directory(folder);
            }
        }
        self.show_project_menu(kind, window, cx);
    }

    fn read_workshop_directory(&mut self, path: PathBuf) {
        let resources = &mut self.shell.tabs.resources;
        resources.directory = Some(path.clone());
        resources.error = std::fs::read_dir(&path).err().map(|_| {
            "This folder could not be read. Return to the folder menu to choose another.".into()
        });
        if resources.error.is_some() {
            return;
        }
        if resources.tree.is_none() {
            if let Some(root) = resources.root.clone() {
                resources.tree = Some(FileTreeState::reader(root));
            }
        }
        if let Some(tree) = &mut resources.tree {
            tree.children_of(&path);
        }
    }

    pub(crate) fn workshop_resource_menu(
        &self,
        mut menu: ui::Menu,
        kind: WorkshopMenu,
        cx: &mut Context<Self>,
    ) -> ui::Menu {
        menu = menu
            .item(
                ui::MenuItem::new("Back to folder menu").on_click(cx.listener(
                    |app, _, window, cx| app.show_project_menu(WorkshopMenu::Project, window, cx),
                )),
            )
            .separator();
        if kind != WorkshopMenu::RecentSessions
            && self.shell.tabs.resources.owner.as_deref() != Some(self.sidebar.active_id())
        {
            return menu.item(
                ui::MenuItem::new("Return to this thread's folder menu").disabled(
                    "The selected thread changed. Reopen Files or Outputs from its folder menu.",
                ),
            );
        }
        match kind {
            WorkshopMenu::Files => {
                let resources = &self.shell.tabs.resources;
                let Some(directory) = resources.directory.clone() else {
                    return menu;
                };
                menu = menu.item(
                    ui::MenuItem::new(format!("Files in {}", directory.display()))
                        .disabled("The folder being browsed."),
                );
                if resources.root.as_ref() != Some(&directory) {
                    if let Some(parent) = directory.parent().map(PathBuf::from) {
                        menu = menu.item(
                            ui::MenuItem::new("Parent folder")
                                .icon(Icon::Folder)
                                .on_click(cx.listener(move |app, _, window, cx| {
                                    app.read_workshop_directory(parent.clone());
                                    app.show_project_menu(WorkshopMenu::Files, window, cx);
                                })),
                        );
                    }
                }
                let refresh_directory = directory.clone();
                menu = menu
                    .item(ui::MenuItem::new("Refresh files").on_click(cx.listener(
                        move |app, _, window, cx| {
                            if let Some(tree) = &mut app.shell.tabs.resources.tree {
                                tree.reload();
                            }
                            app.read_workshop_directory(refresh_directory.clone());
                            app.show_project_menu(WorkshopMenu::Files, window, cx);
                        },
                    )))
                    .item(
                        ui::MenuItem::new(format!(
                            "Open this folder in {}",
                            crate::agent_bin::file_manager_name()
                        ))
                            .icon(Icon::Folder)
                            .on_click(move |_, _, _| {
                                let _ = crate::agent_bin::open_path(std::path::Path::new(&directory));
                            }),
                    )
                    .separator();
                if let Some(error) = &resources.error {
                    return menu.item(ui::MenuItem::new(error.clone()).disabled(error.clone()));
                }
                let entries = resources
                    .tree
                    .as_ref()
                    .map(|tree| {
                        let path = resources.directory.as_ref().unwrap();
                        tree.loaded_entries(path)
                            .iter()
                            .filter(|entry| tree.is_visible(entry))
                            .cloned()
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if entries.is_empty() {
                    menu = menu.item(
                        ui::MenuItem::new("No visible files")
                            .disabled("This folder has no visible files."),
                    );
                }
                for entry in entries {
                    let path = entry.path;
                    let item = ui::MenuItem::new(if entry.is_dir {
                        format!("{} /", entry.name)
                    } else {
                        entry.name.clone()
                    })
                    .icon(if entry.is_dir {
                        Icon::Folder
                    } else {
                        Icon::for_extension(entry.name.rsplit('.').next().unwrap_or(""))
                    });
                    menu = menu.item(if entry.is_dir {
                        item.on_click(cx.listener(move |app, _, window, cx| {
                            app.read_workshop_directory(path.clone());
                            app.show_project_menu(WorkshopMenu::Files, window, cx);
                        }))
                    } else {
                        item.on_click(move |_, _, _| {
                            let _ = crate::agent_bin::open_path(std::path::Path::new(&path));
                        })
                    });
                }
            }
            WorkshopMenu::Outputs => {
                let resources = &self.shell.tabs.resources;
                menu = menu.item(ui::MenuItem::new("Refresh outputs").on_click(cx.listener(
                    |app, _, window, cx| {
                        app.open_workshop_resources(WorkshopMenu::Outputs, window, cx)
                    },
                )));
                if let Some(root) = &resources.root {
                    menu = menu.item(
                        ui::MenuItem::new(format!("Outputs in {}", root.display()))
                            .disabled("The folder captured for these outputs."),
                    );
                }
                if resources.outputs.is_empty() {
                    menu = menu.item(
                        ui::MenuItem::new("No outputs for this thread")
                            .disabled("No output files were recorded for the selected thread."),
                    );
                }
                for (label, path) in &resources.outputs {
                    let path = path.clone();
                    menu = menu.item(
                        ui::MenuItem::new(label.clone())
                            .icon(Icon::for_extension(label.rsplit('.').next().unwrap_or("")))
                            .on_click(move |_, _, _| {
                                let _ = crate::agent_bin::open_path(std::path::Path::new(&path));
                            }),
                    );
                }
            }
            WorkshopMenu::RecentSessions => {
                menu = menu
                    .item(
                        ui::MenuItem::new("Open Sessions").on_click(
                            cx.listener(|app, _, _, cx| app.set_mode(Mode::Sessions, cx)),
                        ),
                    )
                    .item(ui::MenuItem::new("Refresh sessions").on_click(cx.listener(
                        |app, _, window, cx| {
                            app.refresh_sessions(cx);
                            app.show_project_menu(WorkshopMenu::RecentSessions, window, cx);
                        },
                    )))
                    .separator();
                if self.work.hermes_sessions.is_empty() {
                    menu = menu.item(
                        ui::MenuItem::new("No sessions loaded")
                            .disabled("Refresh sessions or open Sessions to browse history."),
                    );
                }
                for session in self.work.hermes_sessions.iter().take(20) {
                    let sid = session.id.clone();
                    let title = session
                        .title
                        .as_deref()
                        .or(session.preview.as_deref())
                        .unwrap_or(&session.id);
                    let ago = session
                        .last_active
                        .or(session.started_at)
                        .map(relative_time)
                        .unwrap_or_default();
                    let detail = format!(
                        "{} · {} · {} msgs · {} tools",
                        ago,
                        session.model.as_deref().unwrap_or("unknown"),
                        session.message_count.unwrap_or(0),
                        session.tool_call_count.unwrap_or(0)
                    );
                    let icon = match session.source.as_deref() {
                        Some("cron") => Icon::Clock,
                        Some("gateway") => Icon::Plug,
                        Some("api") => Icon::Gear,
                        _ => Icon::Circle,
                    };
                    menu = menu.item(
                        ui::MenuItem::new(format!("{title} · {detail}"))
                            .icon(icon)
                            .on_click(cx.listener(move |app, _, _, cx| {
                                app.sessions_state.sessions = app.work.hermes_sessions.clone();
                                app.sessions_state.view = SessionsView::Hermes;
                                app.sessions_state.filter.clear();
                                app.set_mode(Mode::Sessions, cx);
                                app.sessions_state.selected_id = Some(sid.clone());
                                app.sessions_state.transcript = None;
                                app.sessions_state.transcript_error = None;
                                app.sessions_focus_pending = Some(crate::SessionsFocusTarget::Back);
                                app.fetch_session_transcript(sid.clone(), cx);
                                cx.notify();
                            })),
                    );
                }
            }
            _ => {}
        }
        menu
    }
}
