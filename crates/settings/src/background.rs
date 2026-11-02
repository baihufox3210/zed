use crate::{BackgroundPosition, BackgroundSettingsContent, BackgroundSize, SettingsStore};
use gpui::{AnyElement, App, Hsla, ReadGlobal as _, prelude::*};

#[derive(Clone, Copy)]
pub enum Region {
    Editor,
    Panels,
    Assistant,
    Terminal,
    Chrome,
}

impl Region {
    const ALL: [Region; 5] = [
        Region::Editor,
        Region::Panels,
        Region::Assistant,
        Region::Terminal,
        Region::Chrome,
    ];
}

/// The background settings, only when an image is set and shown.
fn active(cx: &App) -> Option<&BackgroundSettingsContent> {
    SettingsStore::global(cx)
        .merged_settings()
        .background
        .as_ref()
        .filter(|settings| settings.enabled == Some(true) && settings.image_path.is_some())
}

pub fn is_enabled(cx: &App) -> bool {
    active(cx).is_some_and(|settings| {
        Region::ALL
            .into_iter()
            .any(|region| selected(settings, region))
    })
}

pub fn includes(region: Region, cx: &App) -> bool {
    active(cx).is_some_and(|settings| selected(settings, region))
}

fn selected(settings: &BackgroundSettingsContent, region: Region) -> bool {
    match settings.scope.unwrap_or_default() {
        crate::BackgroundScope::Window => true,
        crate::BackgroundScope::Editor => matches!(region, Region::Editor),
        crate::BackgroundScope::Custom => match region {
            Region::Editor => settings.editor.unwrap_or(true),
            Region::Panels => settings.panels.unwrap_or(true),
            Region::Assistant => settings.assistant.unwrap_or(true),
            Region::Terminal => settings.terminal.unwrap_or(true),
            Region::Chrome => settings.chrome.unwrap_or(true),
        },
    }
}

fn fraction(value: Option<f32>, default: f32) -> f32 {
    value.unwrap_or(default).clamp(0.0, 1.0)
}

pub fn region_color(color: Hsla, region: Region, cx: &App) -> Hsla {
    if includes(region, cx) {
        color.opacity(0.0)
    } else {
        color
    }
}

pub fn render_window(color: Hsla, cx: &App) -> Option<AnyElement> {
    if !is_enabled(cx) {
        return None;
    }
    render_image(color, cx)
}

pub fn surface_color(color: Hsla, cx: &App) -> Hsla {
    region_color(color, Region::Panels, cx)
}

pub fn chrome_surface_color(color: Hsla, region: Region, cx: &App) -> Hsla {
    match active(cx) {
        Some(settings) if selected(settings, region) => {
            color.opacity(fraction(settings.surface_opacity, 0.25))
        }
        _ => color,
    }
}

pub fn message_color(color: Hsla, cx: &App) -> Hsla {
    match active(cx) {
        Some(settings) if selected(settings, Region::Assistant) => {
            color.opacity(fraction(settings.message_opacity, 0.8))
        }
        _ => color,
    }
}

fn render_image(color: Hsla, cx: &App) -> Option<AnyElement> {
    let settings = active(cx)?;
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
                    .image_blur(settings.blur.unwrap_or(0.0).clamp(0.0, 30.0))
                    .opacity(fraction(settings.image_opacity, 1.0)),
            )
            .child(
                gpui::div()
                    .absolute()
                    .inset_0()
                    .size_full()
                    .bg(gpui::black().opacity(fraction(settings.overlay_opacity, 0.0))),
            )
            .child(
                gpui::div()
                    .absolute()
                    .inset_0()
                    .size_full()
                    .bg(color.opacity(fraction(settings.editor_opacity, 0.8))),
            )
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BackgroundScope;

    #[test]
    fn scope_selects_regions_without_losing_custom_choices() {
        let mut settings = BackgroundSettingsContent {
            scope: Some(BackgroundScope::Custom),
            editor: Some(false),
            panels: Some(true),
            assistant: Some(false),
            terminal: Some(true),
            chrome: Some(false),
            ..Default::default()
        };
        let regions = Region::ALL;
        assert_eq!(
            regions.map(|region| selected(&settings, region)),
            [false, true, false, true, false]
        );
        settings.scope = Some(BackgroundScope::Window);
        assert!(
            regions
                .into_iter()
                .all(|region| selected(&settings, region))
        );
        settings.scope = Some(BackgroundScope::Editor);
        assert_eq!(
            regions.map(|region| selected(&settings, region)),
            [true, false, false, false, false]
        );
        settings.scope = Some(BackgroundScope::Custom);
        assert_eq!(
            regions.map(|region| selected(&settings, region)),
            [false, true, false, true, false]
        );
    }
}
