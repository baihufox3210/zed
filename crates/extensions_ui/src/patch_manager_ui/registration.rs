use std::path::PathBuf;

use gpui::{Context, Entity, FocusHandle, Focusable, Window};
use patch_manager::{CATEGORIES, Patch};
use ui::prelude::*;
use ui_input::InputField;
use util::ResultExt;

use super::{Operation, PatchManagerPage};

pub(super) struct Registration {
    source: Option<PathBuf>,
    identifier: Entity<InputField>,
    name: Entity<InputField>,
    description: Entity<InputField>,
    category: usize,
    enabled: bool,
}

impl Registration {
    pub fn new(window: &mut Window, cx: &mut Context<PatchManagerPage>) -> Self {
        Self {
            source: None,
            identifier: cx.new(|cx| InputField::new(window, cx, "my-patch").label("Patch ID")),
            name: cx.new(|cx| InputField::new(window, cx, "Patch name").label("Name")),
            description: cx.new(|cx| {
                InputField::new(window, cx, "What does this patch change?").label("Description")
            }),
            category: 0,
            enabled: false,
        }
    }

    pub fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        self.identifier.read(cx).focus_handle(cx)
    }

    fn choose_file(cx: &mut Context<PatchManagerPage>) {
        let prompt = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Select a .patch file".into()),
        });
        cx.spawn(async move |page, cx| {
            let result = prompt.await;
            page.update(cx, |page, cx| {
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            if path
                                .extension()
                                .is_some_and(|extension| extension == "patch")
                            {
                                if let Some(registration) = &mut page.registration {
                                    registration.source = Some(path);
                                }
                                page.error = None;
                            } else {
                                page.error = Some("Select a .patch file".into());
                            }
                        }
                    }
                    Ok(Ok(None)) => {}
                    Ok(Err(error)) => {
                        page.error = Some(format!("Could not open file picker: {error:#}"))
                    }
                    Err(error) => page.error = Some(error.to_string()),
                }
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    pub fn render(&self, busy: bool, cx: &mut Context<PatchManagerPage>) -> AnyElement {
        v_flex().gap_3()
            .child(Headline::new("Register Patch").size(HeadlineSize::Medium))
            .child(Label::new("Registration validates and records the patch. Apply is a separate, confirmed source operation.").color(Color::Muted))
            .child(Button::new("choose-patch", "Choose .patch File").disabled(busy)
                .on_click(cx.listener(|_, _, _, cx| Self::choose_file(cx))))
            .when_some(self.source.as_ref(), |element, source| element.child(Label::new(source.display().to_string())))
            .child(self.identifier.clone()).child(self.name.clone()).child(self.description.clone())
            .child(Label::new("Category").color(Color::Muted))
            .child(h_flex().flex_wrap().gap_2().children(CATEGORIES.iter().enumerate().map(|(index, category)| {
                Button::new(*category, *category).toggle_state(self.category == index).disabled(busy)
                    .on_click(cx.listener(move |page, _, _, cx| {
                        if let Some(registration) = &mut page.registration { registration.category = index; }
                        cx.notify();
                    }))
            })))
            .child(Button::new("enabled-registration", "Enable after registration (does not apply)")
                .toggle_state(self.enabled).disabled(busy).on_click(cx.listener(|page, _, _, cx| {
                    if let Some(registration) = &mut page.registration { registration.enabled = !registration.enabled; }
                    cx.notify();
                })))
            .child(h_flex().gap_2()
                .child(Button::new("submit-registration", "Validate and Register").disabled(busy || self.source.is_none())
                    .on_click(cx.listener(|page, _, _, cx| {
                        let Some(registration) = &page.registration else { return; };
                        let Some(source) = registration.source.clone() else { return; };
                        let Some(category) = CATEGORIES.get(registration.category) else { return; };
                        let patch = Patch {
                            id: registration.identifier.read(cx).text(cx).trim().into(),
                            name: registration.name.read(cx).text(cx).trim().into(),
                            description: registration.description.read(cx).text(cx).trim().into(),
                            category: (*category).into(), source: String::new(), sha256: String::new(),
                            enabled: registration.enabled,
                            rebuild_required: false,
                        };
                        page.run(Operation::Register(source, patch), cx);
                    })))
                .child(Button::new("cancel-registration", "Cancel").disabled(busy)
                    .on_click(cx.listener(|page, _, _, cx| { page.registration = None; page.error = None; cx.notify(); }))))
            .into_any_element()
    }
}
