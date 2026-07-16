use gpui::WindowOptions;

pub(super) fn workbench_window(title: &'static str) -> WindowOptions {
    WindowOptions {
        titlebar: Some(gpui::TitlebarOptions {
            title: Some(title.into()),
            appears_transparent: true,
            ..Default::default()
        }),
        ..Default::default()
    }
}
