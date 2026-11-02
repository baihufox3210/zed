use editor::Editor;
use gpui::ReadGlobal as _;
use gpui::{ScrollHandle, prelude::*};
use language::Buffer;
use settings::SettingsStore;
use ui::prelude::*;

use crate::SettingsWindow;

pub(crate) fn render_background_preview(
    _: &SettingsWindow,
    _: &ScrollHandle,
    window: &mut Window,
    cx: &mut Context<SettingsWindow>,
) -> AnyElement {
    let editor = window.use_keyed_state("background-preview-editor", cx, |window, cx| {
        let buffer = cx.new(|cx| {
            Buffer::local(
                "// Background preview\n\nfn main() {\n    println!(\"Hello, Zed!\");\n}\n",
                cx,
            )
        });
        Editor::for_buffer(buffer, None, window, cx)
    });
    let path = SettingsStore::global(cx)
        .merged_settings()
        .background
        .as_ref()
        .and_then(|background| background.image_path.as_deref());
    let name = path
        .and_then(|path| std::path::Path::new(path).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "No image selected".into());
    v_flex()
        .size_full()
        .p_8()
        .gap_4()
        .child(
            h_flex()
                .gap_3()
                .when_some(path, |row, path| {
                    row.child(
                        gpui::img(std::path::PathBuf::from(path))
                            .w(gpui::px(96.0))
                            .h(gpui::px(64.0))
                            .object_fit(gpui::ObjectFit::Contain),
                    )
                })
                .child(Label::new(name)),
        )
        .child(
            div()
                .relative()
                .w_full()
                .h(gpui::px(360.0))
                .overflow_hidden()
                .children(settings::background::render_window(
                    cx.theme().colors().editor_background,
                    cx,
                ))
                .child(editor),
        )
        .into_any_element()
}
