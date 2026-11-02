use crate::{BackgroundPosition, BackgroundSize, SettingsStore};
use gpui::{AnyElement, App, Hsla, ReadGlobal as _, prelude::*};

pub fn is_enabled(cx: &App) -> bool {
    SettingsStore::global(cx)
        .merged_settings()
        .background
        .as_ref()
        .is_some_and(|settings| settings.enabled == Some(true) && settings.image_path.is_some())
}

pub fn surface_color(color: Hsla, cx: &App) -> Hsla {
    if is_enabled(cx) {
        color.opacity(0.0)
    } else {
        color
    }
}

pub fn render_surface(color: Hsla, cx: &App) -> Option<AnyElement> {
    let settings = SettingsStore::global(cx)
        .merged_settings()
        .background
        .as_ref()?;
    if settings.enabled != Some(true) {
        return None;
    }
    let path = std::path::PathBuf::from(settings.image_path.as_ref()?);
    let fit = match settings.size.unwrap_or_default() {
        BackgroundSize::Cover => gpui::ObjectFit::Cover,
        BackgroundSize::Contain => gpui::ObjectFit::Contain,
        BackgroundSize::Stretch => gpui::ObjectFit::Fill,
    };
    let (horizontal, vertical) = match settings.position.unwrap_or_default() {
        BackgroundPosition::Center => (0.5, 0.5),
        BackgroundPosition::Top => (0.5, 0.0),
        BackgroundPosition::Bottom => (0.5, 1.0),
        BackgroundPosition::Left => (0.0, 0.5),
        BackgroundPosition::Right => (1.0, 0.5),
    };
    Some(
        gpui::div()
            .absolute()
            .inset_0()
            .size_full()
            .overflow_hidden()
            .bg(color)
            .child(
                gpui::img(path)
                    .absolute()
                    .inset_0()
                    .size_full()
                    .object_fit(fit)
                    .image_position(horizontal, vertical)
                    .image_blur(settings.blur.unwrap_or(0.0))
                    .opacity(settings.image_opacity.unwrap_or(1.0).clamp(0.0, 1.0)),
            )
            .child(
                gpui::div()
                    .absolute()
                    .inset_0()
                    .size_full()
                    .bg(gpui::black()
                        .opacity(settings.overlay_opacity.unwrap_or(0.0).clamp(0.0, 1.0))),
            )
            .child(
                gpui::div()
                    .absolute()
                    .inset_0()
                    .size_full()
                    .bg(color.opacity(settings.editor_opacity.unwrap_or(0.8).clamp(0.0, 1.0))),
            )
            .into_any_element(),
    )
}
