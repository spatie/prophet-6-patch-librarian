#[cfg(target_os = "macos")]
mod about;
mod app;
mod library;
mod midi;
mod quit;
mod session;
mod sysex;
mod view;

#[cfg(target_os = "macos")]
use gpui_kit::component::TitleBar;
use gpui_kit::{
    component::{Theme, ThemeMode},
    *,
};

const TITLE: &str = "Prophet-6 Patch Librarian";

actions!(
    librarian,
    [About, Quit, Import, Export, Preview, AutoPreview, Search]
);
#[cfg(target_os = "macos")]
actions!(librarian, [Hide, HideOthers, ShowAll, Minimize, Zoom]);

struct MainWindow(AnyWindowHandle, Entity<app::Librarian>);

impl Global for MainWindow {}

fn main() {
    let application = gpui_kit::application().with_assets(assets::Assets);
    #[cfg(target_os = "macos")]
    quit::native::install();
    #[cfg(target_os = "macos")]
    application.on_reopen(|cx| {
        let MainWindow(handle, librarian) = cx.global::<MainWindow>();
        let librarian = librarian.clone();
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_err()
        {
            cx.global_mut::<MainWindow>().0 = open_main_window(Some(librarian), cx).0;
        }
    });

    application.run(|cx| {
        #[cfg(target_os = "macos")]
        quit::native::init(cx);
        gpui_kit::init(cx);
        let shortcuts: Vec<(&str, Box<dyn Action>)> = vec![
            ("secondary-q", Box::new(Quit)),
            ("secondary-o", Box::new(Import)),
            ("secondary-shift-s", Box::new(Export)),
            ("secondary-p", Box::new(Preview)),
            ("secondary-shift-p", Box::new(AutoPreview)),
            ("secondary-f", Box::new(Search)),
            #[cfg(target_os = "macos")]
            ("secondary-h", Box::new(Hide)),
            #[cfg(target_os = "macos")]
            ("secondary-alt-h", Box::new(HideOthers)),
            #[cfg(target_os = "macos")]
            ("secondary-m", Box::new(Minimize)),
        ];
        let bindings = shortcuts
            .into_iter()
            .map(|(key, action)| {
                KeyBinding::load(key, action, None, false, None, cx).expect("Invalid app shortcut")
            })
            .collect::<Vec<_>>();
        cx.bind_keys(bindings);
        cx.set_menus([
            Menu {
                name: TITLE.into(),
                disabled: false,
                items: vec![
                    #[cfg(target_os = "macos")]
                    MenuItem::action("About Prophet-6 Patch Librarian", About),
                    #[cfg(target_os = "macos")]
                    MenuItem::separator(),
                    #[cfg(target_os = "macos")]
                    MenuItem::action("Hide Prophet-6 Patch Librarian", Hide),
                    #[cfg(target_os = "macos")]
                    MenuItem::action("Hide Others", HideOthers),
                    #[cfg(target_os = "macos")]
                    MenuItem::action("Show All", ShowAll),
                    #[cfg(target_os = "macos")]
                    MenuItem::separator(),
                    MenuItem::action("Quit Patch Librarian", Quit),
                ],
            },
            Menu {
                name: "File".into(),
                disabled: false,
                items: vec![
                    MenuItem::action("Add .syx files…", Import),
                    MenuItem::action("Export SysEx…", Export),
                    MenuItem::separator(),
                    MenuItem::action("Search patches", Search),
                ],
            },
            Menu::new("Patch").items([
                MenuItem::action("Preview patch", Preview),
                MenuItem::action("Toggle auto preview", AutoPreview),
            ]),
            #[cfg(target_os = "macos")]
            Menu::new("Edit").items([
                MenuItem::os_action("Undo", component::input::Undo, OsAction::Undo),
                MenuItem::os_action("Redo", component::input::Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", component::input::Cut, OsAction::Cut),
                MenuItem::os_action("Copy", component::input::Copy, OsAction::Copy),
                MenuItem::os_action("Paste", component::input::Paste, OsAction::Paste),
                MenuItem::separator(),
                MenuItem::os_action(
                    "Select All",
                    component::input::SelectAll,
                    OsAction::SelectAll,
                ),
            ]),
            #[cfg(target_os = "macos")]
            Menu::new("Window").items([
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
            ]),
        ]);
        #[cfg(target_os = "macos")]
        {
            let about = std::rc::Rc::new(std::cell::Cell::new(None::<AnyWindowHandle>));
            cx.on_action(move |_: &About, cx| {
                let about = about.clone();
                cx.defer(move |cx| {
                    if let Some(handle) = about.get() {
                        if handle
                            .update(cx, |_, window, _| window.activate_window())
                            .is_ok()
                        {
                            return;
                        }
                    }
                    about.set(Some(about::open(cx)));
                });
            });
            cx.on_action(|_: &Hide, cx| cx.hide());
            cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
            cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
            cx.on_action(|_: &Minimize, cx| {
                if let Some(handle) = cx.active_window() {
                    cx.defer(move |cx| {
                        let _ = handle.update(cx, |_, window, _| window.minimize_window());
                    });
                }
            });
            cx.on_action(|_: &Zoom, cx| {
                if let Some(handle) = cx.active_window() {
                    cx.defer(move |cx| {
                        let _ = handle.update(cx, |_, window, _| window.zoom_window());
                    });
                }
            });
        }
        cx.on_action(|_: &Quit, cx| {
            cx.defer(request_quit);
        });
        let (handle, librarian) = open_main_window(None, cx);
        cx.set_global(MainWindow(handle, librarian));
        cx.activate(true);
    });
}

fn request_quit(cx: &mut App) {
    let MainWindow(handle, librarian) = cx.global::<MainWindow>();
    let mut handle = *handle;
    let librarian = librarian.clone();
    if handle
        .update(cx, |_, window, _| window.activate_window())
        .is_err()
    {
        if !librarian.read(cx).collection().needs_export() {
            quit::approved(cx);
            return;
        }
        handle = open_main_window(Some(librarian.clone()), cx).0;
        cx.global_mut::<MainWindow>().0 = handle;
    }
    let _ = handle.update(cx, |_, window, cx| {
        librarian.update(cx, |view, cx| {
            view.request_close(app::CloseIntent::Quit, window, cx)
        });
    });
}

fn open_main_window(
    librarian: Option<Entity<app::Librarian>>,
    cx: &mut App,
) -> (AnyWindowHandle, Entity<app::Librarian>) {
    #[cfg(target_os = "macos")]
    let options = TitleBar::window_options();
    #[cfg(not(target_os = "macos"))]
    let options = WindowOptions::default();
    gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1100.), px(700.)), cx)),
            window_min_size: Some(size(px(880.), px(590.))),
            titlebar: Some(TitlebarOptions {
                title: Some(TITLE.into()),
                #[cfg(target_os = "macos")]
                traffic_light_position: Some(point(px(14.), px(20.))),
                ..options.titlebar.unwrap_or_default()
            }),
            ..options
        },
        cx,
        |window, cx| {
            let librarian =
                librarian.unwrap_or_else(|| cx.new(|cx| app::Librarian::new(window, cx)));
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
            let for_close = librarian.downgrade();
            window.on_window_should_close(cx, move |window, cx| {
                if let Some(view) = for_close.upgrade() {
                    view.update(cx, |view, cx| {
                        view.request_close(app::CloseIntent::Window, window, cx)
                    });
                    false
                } else {
                    true
                }
            });
            librarian
        },
    )
    .expect("Could not open Patch Librarian")
}
