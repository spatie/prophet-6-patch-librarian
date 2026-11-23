use gpui_kit::{
    component::{
        ActiveTheme, Sizable,
        button::{Button, ButtonVariants},
    },
    *,
};
use std::{sync::Arc, time::Duration};

pub(crate) fn open(cx: &mut App) -> AnyWindowHandle {
    cx.set_reduce_motion(
        objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion(),
    );
    gpui_kit::open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(568.), px(504.)), cx)),
            titlebar: Some(TitlebarOptions {
                title: Some(format!("About {}", crate::TITLE).into()),
                ..Default::default()
            }),
            is_resizable: false,
            is_minimizable: false,
            ..Default::default()
        },
        cx,
        |_, cx| {
            cx.new(|_| About {
                icon: Arc::new(Image::from_bytes(
                    ImageFormat::Png,
                    include_bytes!("../assets/AppIcon.png").to_vec(),
                )),
                spatie: Arc::new(Image::from_bytes(
                    ImageFormat::Svg,
                    include_bytes!("../assets/spatie.svg").to_vec(),
                )),
                landscape: Arc::new(Image::from_bytes(
                    ImageFormat::Svg,
                    include_bytes!("../assets/postcard-landscape.svg").to_vec(),
                )),
                note: Arc::new(Image::from_bytes(
                    ImageFormat::Svg,
                    include_bytes!("../assets/postcard-note.svg").to_vec(),
                )),
            })
        },
    )
    .expect("Could not open About Patch Librarian")
    .0
}

struct About {
    icon: Arc<Image>,
    spatie: Arc<Image>,
    landscape: Arc<Image>,
    note: Arc<Image>,
}

impl Render for About {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.foreground)
            .px_8().pt_5().pb_7()
            .flex().flex_col().gap_5()
            .child(div().flex().items_center().gap_5()
                .child(img(self.icon.clone()).size(px(80.)))
                .child(div().flex().flex_col().gap_1()
                    .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(crate::TITLE))
                    .child(div().text_sm().text_color(theme.muted_foreground)
                        .child(format!("Version {}", env!("CARGO_PKG_VERSION"))))))
            .child(div().text_sm().line_height(px(21.)).child(
                "Back up and organize Prophet-6 patches, and preview them over USB."))
            .child(div().flex().items_center().gap_4()
                .child(img(self.spatie.clone()).w(px(98.)).h(px(42.)))
                .child(div().flex_1().flex().flex_col().gap_1()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Made by Spatie"))
                    .child(div().text_sm().text_color(theme.muted_foreground)
                        .child("A web development agency in Antwerp, Belgium."))))
            .child(div().rounded_xl().border_1().border_color(rgb(0xe4dbcd)).bg(rgb(0xf3eee5)).p_4()
                .flex().items_center().gap_4()
                    .child(div().relative().flex_none().w(px(132.)).h(px(112.))
                        .child(div().absolute().top_0().w(px(128.)).h(px(100.))
                            .child(img(self.landscape.clone()).size_full())
                            .with_animation("postcard-landscape", Animation::new(Duration::from_millis(420)).with_easing(ease_out_quint()),
                                |card, delta| card.left(px(12. - 20. * delta))))
                        .child(div().absolute().w(px(128.)).h(px(100.))
                            .child(img(self.note.clone()).size_full())
                            .with_animation("postcard-note", Animation::new(Duration::from_millis(420)).with_easing(ease_out_quint()),
                                |card, delta| card.left(px(2. + 12. * delta)).top(px(2. + 8. * delta)))))
                    .child(div().w(px(320.)).flex_none().flex().flex_col().gap_2()
                        .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Postcardware"))
                        .child(div().w_full().text_sm().line_height(px(20.)).child(
                            "If it earns a spot next to your Prophet-6, we’d love a postcard from your hometown."))
                        .child(div().flex().items_center().gap_4()
                            .child(link("mailing-address", "Mailing address", "https://spatie.be/about-us"))
                            .child(link("postcard-wall", "Postcard wall", "https://spatie.be/open-source/postcards")))))
            .child(div().border_t_1().border_color(theme.border).pt_3()
                .flex().items_center().justify_between()
                .child(div().flex().flex_col().gap_1()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child("Contribute"))
                    .child(div().text_sm().text_color(theme.muted_foreground).child("Pull requests are welcome.")))
                .child(link("github", "View on GitHub", "https://github.com/spatie/prophet-6-patch-librarian")))
    }
}

fn link(id: &'static str, label: &'static str, url: &'static str) -> Button {
    Button::new(id)
        .link()
        .small()
        .label(label)
        .tooltip(url)
        .on_click(move |_, _, cx| cx.open_url(url))
}
