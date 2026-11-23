mod app;
mod library;
mod midi;
mod sysex;
mod view;

use gpui_kit::{
    component::{Theme, ThemeMode},
    *,
};

const TITLE: &str = "Prophet-6 Patch Librarian";

actions!(librarian, [Quit, Import, Export]);

fn main() {
    gpui_kit::application()
        .with_assets(assets::Assets)
        .run(|cx| {
            gpui_kit::init(cx);
            let shortcuts: [(&str, Box<dyn Action>); 3] = [
                ("secondary-q", Box::new(Quit)),
                ("secondary-o", Box::new(Import)),
                ("secondary-shift-s", Box::new(Export)),
            ];
            let bindings = shortcuts
                .into_iter()
                .map(|(key, action)| {
                    KeyBinding::load(key, action, None, false, None, cx)
                        .expect("Invalid app shortcut")
                })
                .collect::<Vec<_>>();
            cx.bind_keys(bindings);
            cx.set_menus([
                Menu {
                    name: TITLE.into(),
                    disabled: false,
                    items: vec![MenuItem::action("Quit Patch Librarian", Quit)],
                },
                Menu {
                    name: "File".into(),
                    disabled: false,
                    items: vec![
                        MenuItem::action("Add .syx files…", Import),
                        MenuItem::action("Export SysEx…", Export),
                    ],
                },
            ]);
            gpui_kit::open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::centered(size(px(1100.), px(700.)), cx)),
                    window_min_size: Some(size(px(880.), px(590.))),
                    titlebar: Some(TitlebarOptions {
                        title: Some(TITLE.into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                cx,
                |window, cx| {
                    Theme::change(ThemeMode::Light, Some(window), cx);
                    Theme::update(cx, |theme| {
                        theme.background = rgb(0xffffff).into();
                        theme.sidebar = rgb(0xf8f7f5).into();
                        theme.foreground = rgb(0x22201f).into();
                        theme.muted_foreground = rgb(0x625e5a).into();
                        theme.border = rgb(0xe3dfda).into();
                        theme.primary = rgb(0xa63d2b).into();
                        theme.primary_foreground = rgb(0xffffff).into();
                        theme.ring = rgb(0xb64935).into();
                    });
                    let librarian = cx.new(|cx| app::Librarian::new(window, cx));
                    let for_close = librarian.downgrade();
                    let for_quit = librarian.downgrade();
                    window.on_window_should_close(cx, move |_, cx| {
                        for_close
                            .upgrade()
                            .is_none_or(|view| view.read(cx).progress.is_none())
                    });
                    cx.on_action(move |_: &Quit, cx| {
                        if for_quit
                            .upgrade()
                            .is_none_or(|view| view.read(cx).progress.is_none())
                        {
                            cx.quit();
                        }
                    });
                    librarian
                },
            )
            .expect("Could not open Patch Librarian");
            cx.activate(true);
        });
}
