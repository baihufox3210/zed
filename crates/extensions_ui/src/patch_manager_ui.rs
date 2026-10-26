mod registration;

use std::path::PathBuf;

use anyhow::{Context as _, Result};
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, Render, WeakEntity, Window, actions,
};
use patch_manager::{Compatibility, PatchService, PatchState};
use ui::{Divider, prelude::*};
use ui_input::InputField;
use util::ResultExt;
use workspace::{
    Workspace,
    item::{Item, ItemEvent},
};

use registration::Registration;

actions!(
    patch_manager,
    [
        /// Opens the native source patch manager.
        Open,
    ]
);

pub(super) fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(|workspace, _: &Open, window, cx| {
            let existing = workspace.panes().iter().find_map(|pane| {
                pane.read(cx)
                    .items()
                    .find_map(|item| item.downcast::<PatchManagerPage>())
            });
            if let Some(existing) = existing {
                existing.update(cx, |page, cx| page.run(Operation::Refresh, cx));
                workspace.activate_item(&existing, true, true, window, cx);
                return;
            }
            let project = workspace.project().read(cx);
            let root = project
                .is_local()
                .then(|| {
                    project
                        .visible_worktrees(cx)
                        .next()
                        .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
                })
                .flatten();
            let workspace_handle = workspace.weak_handle();
            let page = cx.new(|cx| PatchManagerPage::new(root, workspace_handle, window, cx));
            workspace.add_item_to_active_pane(Box::new(page), None, true, window, cx);
        });
    })
    .detach();
}

#[derive(Clone, Copy, PartialEq)]
enum Filter {
    All,
    Enabled,
    Disabled,
    Conflicts,
}

#[derive(Clone)]
enum Operation {
    Refresh,
    Enable(String, bool),
    Apply(String),
    Revert(String),
    Remove(String),
    Register(PathBuf, patch_manager::Patch),
}

impl Operation {
    fn completion(&self) -> &'static str {
        match self {
            Self::Refresh => "Validation complete. Runtime state is unverified.",
            Self::Enable(..) => "Activation saved. Source application is a separate operation.",
            Self::Apply(_) => {
                "Patch applied to source. Rebuild and restart required; runtime state is unverified."
            }
            Self::Revert(_) => {
                "Patch reverted from source. Rebuild and restart required; runtime state is unverified."
            }
            Self::Remove(_) => "Registration removed. The patch file was retained.",
            Self::Register(..) => "Patch registered. Source application is a separate operation.",
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::Refresh => "Validating patches…",
            Self::Enable(..) => "Updating activation…",
            Self::Apply(_) => "Applying patch…",
            Self::Revert(_) => "Reverting patch…",
            Self::Remove(_) => "Removing registration…",
            Self::Register(..) => "Validating and registering patch…",
        }
    }
}

struct PatchManagerPage {
    workspace: WeakEntity<Workspace>,
    root: Option<PathBuf>,
    page_focus: FocusHandle,
    service: Option<PatchService>,
    search: Entity<InputField>,
    patches: Vec<PatchState>,
    selected: Option<String>,
    registration: Option<Registration>,
    filter: Filter,
    busy: bool,
    message: String,
    error: Option<String>,
}

impl PatchManagerPage {
    fn new(
        root: Option<PathBuf>,
        workspace: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            InputField::new(window, cx, "Search patches…").start_icon(IconName::MagnifyingGlass)
        });
        let editor = search.read(cx).editor().clone();
        editor
            .subscribe(
                Box::new({
                    let page = cx.entity().downgrade();
                    move |_, _, cx| {
                        page.update(cx, |_, cx| cx.notify()).log_err();
                    }
                }),
                window,
                cx,
            )
            .detach();
        let page_focus = cx.focus_handle();
        cx.on_focus_in(&page_focus, window, |page, _, cx| {
            if page.registration.is_none() {
                page.run(Operation::Refresh, cx);
            }
        })
        .detach();
        let mut page = Self {
            workspace,
            root,
            page_focus,
            service: None,
            search,
            patches: Vec::new(),
            selected: None,
            registration: None,
            filter: Filter::All,
            busy: false,
            message: String::new(),
            error: None,
        };
        page.run(Operation::Refresh, cx);
        page
    }

    fn run(&mut self, operation: Operation, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if matches!(&operation, Operation::Apply(_) | Operation::Revert(_)) {
            let dirty = self
                .workspace
                .read_with(cx, |workspace, cx| {
                    workspace
                        .project()
                        .read(cx)
                        .buffer_store()
                        .read(cx)
                        .buffers()
                        .any(|buffer| buffer.read(cx).is_dirty())
                })
                .unwrap_or(true);
            if dirty {
                self.error =
                    Some("Save open buffers before applying or reverting source patches.".into());
                cx.notify();
                return;
            }
        }
        self.busy = true;
        self.error = None;
        self.message = operation.label().into();
        cx.notify();
        let service = self.service.clone();
        let root = self.root.clone();
        let registration = matches!(&operation, Operation::Register(..));
        let completion = operation.completion();
        let background = cx.background_spawn(async move {
            let service = match service {
                Some(service) => service,
                None => {
                    PatchService::open(&root.context("Open the local Zed source checkout first")?)?
                }
            };
            let operation_result = match operation {
                Operation::Refresh => Ok(()),
                Operation::Enable(id, enabled) => service.set_enabled(&id, enabled),
                Operation::Apply(id) => service.apply(&id, false),
                Operation::Revert(id) => service.apply(&id, true),
                Operation::Remove(id) => service.remove(&id),
                Operation::Register(path, patch) => service.register(&path, patch),
            };
            let patches = service.list();
            anyhow::Ok((service, patches, operation_result))
        });
        cx.spawn(async move |page, cx| {
            let result = background.await;
            page.update(cx, |page, cx| {
                page.busy = false;
                match result {
                    Ok((service, patches, operation_result)) => {
                        page.root = Some(service.root().to_path_buf());
                        page.service = Some(service);
                        match patches {
                            Ok(patches) => page.patches = patches,
                            Err(error) => {
                                page.error = Some(format!("Could not refresh state: {error:#}"))
                            }
                        }
                        page.message = if operation_result.is_ok() {
                            completion.into()
                        } else {
                            String::new()
                        };
                        if registration && operation_result.is_ok() {
                            page.registration = None;
                        }
                        if let Err(error) = operation_result {
                            page.error = Some(match page.error.take() {
                                Some(refresh_error) => format!("{error:#}\n{refresh_error}"),
                                None => format!("{error:#}"),
                            });
                        }
                        if page.selected.as_ref().is_some_and(|id| {
                            !page.patches.iter().any(|state| &state.patch.id == id)
                        }) {
                            page.selected = None;
                        }
                    }
                    Err(error) => {
                        page.message.clear();
                        page.error = Some(format!("{error:#}"));
                    }
                }
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn confirm(&mut self, operation: Operation, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let (title, detail) = match &operation {
            Operation::Apply(_) => (
                "Apply patch to source?",
                "This modifies files in the displayed checkout. Save affected editor buffers first. A build and restart are required to use the change.",
            ),
            Operation::Revert(_) => (
                "Revert patch from source?",
                "This removes the patch's changes from the displayed checkout. Save affected editor buffers first. A build and restart are required. Other edits may prevent a safe revert.",
            ),
            Operation::Remove(_) => (
                "Remove patch registration?",
                "Only the manifest entry will be removed. The .patch file and all source changes are retained. This does not revert an applied patch; any remaining source changes still require review, build and restart.",
            ),
            _ => return,
        };
        let prompt = window.prompt(
            gpui::PromptLevel::Warning,
            title,
            Some(detail),
            &["Cancel", "Continue"],
            cx,
        );
        cx.spawn(async move |page, cx| match prompt.await {
            Ok(1) => {
                page.update(cx, |page, cx| page.run(operation, cx))
                    .log_err();
            }
            Ok(_) => {}
            Err(error) => {
                page.update(cx, |page, cx| {
                    page.error = Some(error.to_string());
                    cx.notify();
                })
                .log_err();
            }
        })
        .detach();
    }

    fn view_diff(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(service) = self.service.clone() else {
            return;
        };
        self.busy = true;
        self.message = "Preparing diff…".into();
        self.error = None;
        cx.notify();
        let workspace = self.workspace.clone();
        let background = cx.background_spawn(async move { service.preview(&id) });
        cx.spawn_in(window, async move |page, cx| {
            let result: Result<()> = async {
                let preview = background.await?;
                let task = workspace.update_in(cx, |workspace, window, cx| {
                    git_ui::multi_diff_view::MultiDiffView::open(
                        preview.pairs.clone(),
                        workspace,
                        window,
                        cx,
                    )
                })?;
                let diff_view = task.await?;
                workspace.update_in(cx, |workspace, _, cx| {
                    let paths: Vec<_> = preview.pairs.iter().flatten().map(PathBuf::from).collect();
                    let buffers: Vec<_> = workspace
                        .project()
                        .read(cx)
                        .buffer_store()
                        .read(cx)
                        .buffers()
                        .filter(|buffer| {
                            buffer
                                .read(cx)
                                .file()
                                .and_then(|file| file.as_local())
                                .is_some_and(|file| paths.contains(&file.abs_path(cx)))
                        })
                        .collect();
                    for buffer in buffers {
                        buffer.update(cx, |buffer, cx| {
                            buffer.set_capability(language::Capability::ReadOnly, cx)
                        });
                    }
                    App::observe_release(cx, &diff_view, move |_, cx| {
                        cx.background_spawn(async move { drop(preview) }).detach();
                    })
                    .detach();
                })?;
                Ok(())
            }
            .await;
            page.update(cx, |page, cx| {
                page.busy = false;
                page.message.clear();
                match result {
                    Ok(()) => {}
                    Err(error) => page.error = Some(format!("{error:#}")),
                }
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn choose_checkout(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let prompt = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Select the Zed source checkout".into()),
        });
        cx.spawn(async move |page, cx| {
            let result = prompt.await;
            page.update(cx, |page, cx| {
                match result {
                    Ok(Ok(Some(paths))) => {
                        if !page.busy {
                            if let Some(root) = paths.into_iter().next() {
                                page.root = Some(root);
                                page.service = None;
                                page.patches.clear();
                                page.selected = None;
                                page.registration = None;
                                page.run(Operation::Refresh, cx);
                            }
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        page.error = Some(format!("Could not select checkout: {error:#}"))
                    }
                    Err(error) => page.error = Some(error.to_string()),
                }
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn render_detail(&self, state: &PatchState, cx: &mut Context<Self>) -> AnyElement {
        let patch = &state.patch;
        let id = patch.id.clone();
        let enabled = patch.enabled;
        let applied = state.compatibility == Compatibility::AlreadyApplied;
        v_flex()
            .gap_3()
            .child(
                Button::new("back", "All Patches").on_click(cx.listener(|page, _, _, cx| {
                    page.selected = None;
                    cx.notify();
                })),
            )
            .child(Headline::new(patch.name.clone()).size(HeadlineSize::Medium))
            .child(Label::new(patch.id.clone()).color(Color::Muted))
            .child(Label::new(patch.description.clone()))
            .child(Divider::horizontal())
            .children(
                [
                    format!("Category: {}", patch.category),
                    format!("Enabled: {} (application eligibility)", enabled),
                    format!(
                        "Applied to source: {}",
                        match state.compatibility {
                            Compatibility::Compatible => "No",
                            Compatibility::AlreadyApplied => "Yes",
                            Compatibility::Conflict => "Unknown (conflict or validation failure)",
                        }
                    ),
                    format!("Compatibility: {:?}", state.compatibility),
                    format!(
                        "Rebuild required: {} (runtime unverified)",
                        state.rebuild_required
                    ),
                    format!("Patch source: {}", patch.source),
                    "Build / restart: required after apply or revert; runtime state unverified"
                        .into(),
                ]
                .into_iter()
                .map(Label::new),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("enable", if enabled { "Disable" } else { "Enable" })
                            .disabled(self.busy)
                            .on_click(cx.listener({
                                let id = id.clone();
                                move |page, _, _, cx| {
                                    page.run(Operation::Enable(id.clone(), !enabled), cx)
                                }
                            })),
                    )
                    .child(
                        Button::new("validate", "Validate")
                            .disabled(self.busy)
                            .on_click(
                                cx.listener(|page, _, _, cx| page.run(Operation::Refresh, cx)),
                            ),
                    )
                    .child(
                        Button::new("diff", "View Diff")
                            .disabled(self.busy || state.compatibility == Compatibility::Conflict)
                            .on_click(cx.listener({
                                let id = id.clone();
                                move |page, _, window, cx| page.view_diff(id.clone(), window, cx)
                            })),
                    )
                    .child(
                        Button::new("apply", "Apply")
                            .disabled(
                                self.busy
                                    || !enabled
                                    || state.compatibility != Compatibility::Compatible,
                            )
                            .on_click(cx.listener({
                                let id = id.clone();
                                move |page, _, window, cx| {
                                    page.confirm(Operation::Apply(id.clone()), window, cx)
                                }
                            })),
                    )
                    .child(
                        Button::new("revert", "Revert")
                            .disabled(self.busy || !applied)
                            .on_click(cx.listener({
                                let id = id.clone();
                                move |page, _, window, cx| {
                                    page.confirm(Operation::Revert(id.clone()), window, cx)
                                }
                            })),
                    )
                    .child(
                        Button::new("remove", "Remove Registration")
                            .disabled(self.busy)
                            .on_click(cx.listener(move |page, _, window, cx| {
                                page.confirm(Operation::Remove(id.clone()), window, cx)
                            })),
                    ),
            )
            .child(Label::new("Affected Files").color(Color::Muted))
            .children(state.affected_files.iter().cloned().map(Label::new))
            .child(Label::new("Last Validation Result").color(Color::Muted))
            .child(
                div()
                    .whitespace_normal()
                    .child(Label::new(state.validation.clone())),
            )
            .into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let query = self.search.read(cx).text(cx).to_lowercase();
        let states: Vec<_> = self
            .patches
            .iter()
            .filter(|state| {
                let matches_filter = match self.filter {
                    Filter::All => true,
                    Filter::Enabled => state.patch.enabled,
                    Filter::Disabled => !state.patch.enabled,
                    Filter::Conflicts => state.compatibility == Compatibility::Conflict,
                };
                matches_filter
                    && format!(
                        "{} {} {} {}",
                        state.patch.id,
                        state.patch.name,
                        state.patch.description,
                        state.patch.category
                    )
                    .to_lowercase()
                    .contains(&query)
            })
            .collect();
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_2()
                    .child(div().flex_1().min_w_32().child(self.search.clone()))
                    .child(
                        Button::new("register", "Register Patch")
                            .disabled(self.busy || self.service.is_none())
                            .on_click(cx.listener(|page, _, window, cx| {
                                let registration = Registration::new(window, cx);
                                window.focus(&registration.focus_handle(cx), cx);
                                page.registration = Some(registration);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("refresh", "Refresh")
                            .disabled(self.busy)
                            .on_click(
                                cx.listener(|page, _, _, cx| page.run(Operation::Refresh, cx)),
                            ),
                    ),
            )
            .child(
                h_flex().flex_wrap().gap_2().children(
                    [
                        (Filter::All, "All"),
                        (Filter::Enabled, "Enabled"),
                        (Filter::Disabled, "Disabled"),
                        (Filter::Conflicts, "Conflicts"),
                    ]
                    .into_iter()
                    .map(|(filter, label)| {
                        Button::new(label, label)
                            .toggle_state(self.filter == filter)
                            .on_click(cx.listener(move |page, _, _, cx| {
                                page.filter = filter;
                                cx.notify();
                            }))
                    }),
                ),
            )
            .when(states.is_empty(), |element| {
                element.child(Label::new("No matching patches").color(Color::Muted))
            })
            .children(states.into_iter().map(|state| {
                let patch = &state.patch;
                let id = patch.id.clone();
                let enabled = patch.enabled;
                v_flex()
                    .p_4()
                    .gap_2()
                    .border_1()
                    .border_color(cx.theme().colors().border_variant)
                    .rounded_md()
                    .child(
                        h_flex()
                            .flex_wrap()
                            .justify_between()
                            .gap_2()
                            .child(Label::new(patch.name.clone()))
                            .child(
                                Label::new(if enabled { "Enabled" } else { "Disabled" })
                                    .color(Color::Muted),
                            ),
                    )
                    .child(Label::new(patch.description.clone()).color(Color::Muted))
                    .child(
                        h_flex()
                            .flex_wrap()
                            .justify_between()
                            .gap_2()
                            .child(
                                Label::new(format!(
                                    "{} · {}",
                                    patch.category,
                                    match state.compatibility {
                                        Compatibility::Compatible if state.rebuild_required =>
                                            "Not applied · Rebuild required",
                                        Compatibility::Compatible => "Compatible · Not applied",
                                        Compatibility::AlreadyApplied =>
                                            "Applied · Rebuild required",
                                        Compatibility::Conflict => "Validation failed / Conflict",
                                    }
                                ))
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new(
                                            SharedString::from(format!("detail-{id}")),
                                            "Details",
                                        )
                                        .on_click(
                                            cx.listener({
                                                let id = id.clone();
                                                move |page, _, _, cx| {
                                                    page.selected = Some(id.clone());
                                                    cx.notify();
                                                }
                                            }),
                                        ),
                                    )
                                    .child(
                                        Button::new(
                                            SharedString::from(format!("enable-{id}")),
                                            if enabled { "Disable" } else { "Enable" },
                                        )
                                        .disabled(self.busy)
                                        .on_click(
                                            cx.listener(move |page, _, _, cx| {
                                                page.run(
                                                    Operation::Enable(id.clone(), !enabled),
                                                    cx,
                                                )
                                            }),
                                        ),
                                    ),
                            ),
                    )
            }))
            .into_any_element()
    }
}

impl Render for PatchManagerPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = if let Some(registration) = &self.registration {
            registration.render(self.busy, cx)
        } else if let Some(state) = self
            .selected
            .as_ref()
            .and_then(|id| self.patches.iter().find(|state| &state.patch.id == id))
        {
            self.render_detail(state, cx)
        } else {
            self.render_list(cx)
        };
        v_flex()
            .id("patch-manager")
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .track_focus(&self.page_focus)
            .overflow_y_scroll()
            .p_4()
            .gap_3()
            .child(
                h_flex()
                    .flex_wrap()
                    .justify_between()
                    .gap_2()
                    .child(Headline::new("Patch Manager").size(HeadlineSize::Large))
                    .child(
                        Button::new("choose-checkout", "Source Checkout…")
                            .disabled(self.busy)
                            .on_click(cx.listener(|page, _, _, cx| page.choose_checkout(cx))),
                    ),
            )
            .child(Label::new("Manage custom source patches and integrations").color(Color::Muted))
            .when_some(self.root.as_ref(), |element, root| {
                element.child(
                    Label::new(root.display().to_string())
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            })
            .child(Divider::horizontal())
            .when(!self.message.is_empty(), |element| {
                element.child(Label::new(self.message.clone()).color(Color::Muted))
            })
            .when_some(self.error.clone(), |element, error| {
                element.child(Label::new(error).color(Color::Error))
            })
            .child(content)
    }
}

impl EventEmitter<ItemEvent> for PatchManagerPage {}
impl Focusable for PatchManagerPage {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if let Some(registration) = &self.registration {
            registration.focus_handle(cx)
        } else if self.selected.is_some() {
            self.page_focus.clone()
        } else {
            self.search.read(cx).focus_handle(cx)
        }
    }
}
impl Item for PatchManagerPage {
    type Event = ItemEvent;
    fn tab_content_text(&self, _: usize, _: &App) -> SharedString {
        "Patch Manager".into()
    }
    fn show_toolbar(&self) -> bool {
        false
    }
    fn to_item_events(event: &ItemEvent, emit: &mut dyn FnMut(ItemEvent)) {
        emit(*event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use project::{FakeFs, Project};
    use settings::SettingsStore;
    use util::path;
    use workspace::{MultiWorkspace, pane::CloseActiveItem};

    #[gpui::test]
    async fn patch_manager_tab_opens_reuses_closes_and_reopens(cx: &mut TestAppContext) {
        cx.update(|cx| {
            let settings = SettingsStore::test(cx);
            cx.set_global(settings);
            cx.set_global(db::AppDatabase::test_new());
            theme_settings::init(theme::LoadThemes::JustBase, cx);
            editor::init(cx);
            init(cx);
        });
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(
            path!("/patch-manager-test"),
            serde_json::json!({"source.rs": ""}),
        )
        .await;
        let project = Project::test(fs, [path!("/patch-manager-test").as_ref()], cx).await;
        let (multi_workspace, cx) =
            cx.add_window_view(|window, cx| MultiWorkspace::test_new(project, window, cx));
        let workspace = multi_workspace.read_with(cx, |workspace, _| workspace.workspace().clone());
        cx.dispatch_action(Open);
        cx.run_until_parked();
        let page = workspace.read_with(cx, |workspace, cx| {
            let pane = workspace.active_pane().read(cx);
            assert_eq!(pane.items().count(), 1);
            pane.active_item()
                .and_then(|item| item.downcast::<PatchManagerPage>())
        });
        let Some(page) = page else {
            panic!("Patch Manager action did not open a native item");
        };
        page.read_with(cx, |page, _| {
            assert!(!page.busy);
            assert!(page.error.is_some());
        });
        cx.dispatch_action(Open);
        cx.run_until_parked();
        workspace.read_with(cx, |workspace, cx| {
            let pane = workspace.active_pane().read(cx);
            assert_eq!(pane.items().count(), 1);
            assert_eq!(
                pane.active_item().map(|item| item.item_id()),
                Some(page.entity_id())
            );
        });
        let close = workspace.update_in(cx, |workspace, window, cx| {
            workspace.active_pane().update(cx, |pane, cx| {
                pane.close_active_item(&CloseActiveItem::default(), window, cx)
            })
        });
        if let Err(error) = close.await {
            panic!("Could not close Patch Manager: {error:#}");
        }
        cx.dispatch_action(Open);
        cx.run_until_parked();
        workspace.read_with(cx, |workspace, cx| {
            let pane = workspace.active_pane().read(cx);
            assert_eq!(pane.items().count(), 1);
            assert!(
                pane.active_item()
                    .is_some_and(|item| item.downcast::<PatchManagerPage>().is_some())
            );
        });
    }
}
