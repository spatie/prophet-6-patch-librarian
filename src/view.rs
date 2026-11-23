use crate::{
    Export, Import,
    app::{Librarian, Transfer},
    library::{Entry, USER_SLOT_COUNT},
};
#[cfg(target_os = "macos")]
use gpui_kit::component::TitleBar;
use gpui_kit::{
    component::{
        ActiveTheme, Disableable, IconName, Sizable, Theme,
        button::{Button, ButtonVariants},
        list::ListItem,
        progress::Progress,
    },
    prelude::FluentBuilder,
    *,
};
use std::{collections::BTreeSet, sync::atomic::Ordering};

const ROW_HEIGHT: f32 = 38.;

struct DragPatches(BTreeSet<u64>);

struct DragPreview;

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

impl Render for Librarian {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !cx.has_active_drag() {
            self.dragged_ids.clear();
        }
        let theme = cx.theme();
        div()
            .id("librarian")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(theme.sidebar)
            .text_color(theme.foreground)
            .when(self.progress.is_some(), |root| {
                root.capture_key_down(|_, _, cx| cx.stop_propagation())
            })
            .on_action(cx.listener(|this, _: &Import, _, cx| this.import_picker(cx)))
            .on_action(cx.listener(|this, _: &Export, _, cx| this.export(cx)))
            .map(|root| {
                #[cfg(target_os = "macos")]
                let root = root.child(
                    TitleBar::new()
                        .h(px(56.))
                        .pl(px(92.))
                        .pr_4()
                        .bg(theme.sidebar)
                        .border_b_0()
                        .child(self.toolbar(cx)),
                );
                root.child(self.workspace(cx))
            })
            .when_some(self.progress.as_ref(), |root, (transfer, done)| {
                root.child(transfer_overlay(
                    transfer,
                    done.load(Ordering::Relaxed),
                    theme,
                ))
            })
    }
}

impl Librarian {
    fn workspace(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .px_4()
            .pb_4()
            .gap_3()
            .map(|body| {
                #[cfg(not(target_os = "macos"))]
                let body = body
                    .pt_4()
                    .child(self.heading(theme))
                    .child(self.toolbar(cx));
                body
            })
            .child(self.patch_list(cx))
            .when_some(self.error.as_ref(), |root, error| {
                root.child(
                    div()
                        .flex_none()
                        .text_sm()
                        .text_color(theme.danger)
                        .child(error.clone()),
                )
            })
    }

    #[cfg(not(target_os = "macos"))]
    fn heading(&self, theme: &Theme) -> impl IntoElement {
        div()
            .w_full()
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(crate::TITLE),
            )
            .child(self.usb_status(theme))
    }

    fn usb_status(&self, theme: &Theme) -> impl IntoElement {
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_full()
            .bg(if self.connected {
                theme.green.opacity(0.10)
            } else {
                theme.foreground.opacity(0.045)
            })
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(div().size(px(6.)).rounded_full().bg(if self.connected {
                theme.green
            } else {
                theme.muted_foreground
            }))
            .child(if self.connected {
                "USB connected"
            } else {
                "USB disconnected"
            })
    }

    fn toolbar(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let count = self.collection().patches.len();
        let online = self.connected && !self.busy;
        let send_hint = match count.cmp(&USER_SLOT_COUNT) {
            std::cmp::Ordering::Less => "Add patches to fill all 500 user slots",
            std::cmp::Ordering::Greater => "Remove extra patches to fit 500 user slots",
            std::cmp::Ordering::Equal => "Replace all 500 user slots in list order",
        };
        let action = |id, icon| {
            let button = Button::new(id).rounded(px(7.)).icon(icon);
            if cfg!(target_os = "macos") {
                button.ghost().small()
            } else {
                button.outline()
            }
        };
        let preview = action("preview", IconName::Play)
            .when(!cfg!(target_os = "macos"), |button| button.primary())
            .label(if cfg!(target_os = "macos") {
                "Preview"
            } else {
                "Preview patch"
            })
            .tooltip(if self.selected.len() == 1 {
                "Load the patch temporarily, then play it on the Prophet-6"
            } else {
                "Select one patch to preview"
            })
            .disabled(!online || self.selected.len() != 1)
            .on_click(cx.listener(|this, _, window, cx| this.audition(window, cx)));

        div()
                    .w_full()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_6()
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                action("import", IconName::FolderOpen)
                                    .label("Add .syx")
                                    .tooltip("Append one or more .syx files without replacing the list")
                                    .disabled(!self.editable())
                                    .on_click(cx.listener(|this, _, _, cx| this.import_picker(cx))),
                            )
                            .child(
                                action("export", IconName::FileText)
                                    .label(if cfg!(target_os = "macos") {
                                        "Export"
                                    } else if self.selected.is_empty() {
                                        "Export list"
                                    } else {
                                        "Export selection"
                                    })
                                    .tooltip(if self.selected.is_empty() {
                                        "Export list"
                                    } else {
                                        "Export selection"
                                    })
                                    .disabled(self.busy || count == 0)
                                    .on_click(cx.listener(|this, _, _, cx| this.export(cx))),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_4()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        action("read", IconName::ArrowDown)
                                            .label(if cfg!(target_os = "macos") { "Read" } else { "Read from synth" })
                                            .tooltip("Replace the list with the 500 user slots from the Prophet-6")
                                            .disabled(!online)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.read(window, cx)
                                            })),
                                    )
                                    .child(
                                        action("send", IconName::ArrowUp)
                                            .label(if cfg!(target_os = "macos") { "Send" } else { "Send to synth" })
                                            .tooltip(send_hint)
                                            .disabled(!online || count != USER_SLOT_COUNT)
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.write(window, cx)
                                            })),
                                    ),
                            )
                            .child(preview)
                            .when(cfg!(target_os = "macos"), |bar| {
                                bar.child(self.usb_status(theme))
                            }),
                    )
    }

    fn patch_list(&self, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let count = self.collection().patches.len();
        let selected = self.selected.len();
        let view = cx.entity().downgrade();
        div()
            .id("patches-pane")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .rounded_xl()
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .overflow_hidden()
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.import(paths.paths().to_vec(), cx)
            }))
            .when(count == 0, |pane| {
                pane.child(
                    div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .justify_center()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .size(px(56.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_xl()
                                .bg(theme.sidebar)
                                .text_color(theme.muted_foreground)
                                .child(gpui_kit::component::Icon::new(IconName::FileText)),
                        )
                        .child(
                            div()
                                .text_lg()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("No patches in the list"),
                        )
                        .child(div().text_sm().text_color(theme.muted_foreground).child(
                            if self.connected {
                                "Add .syx files or read the user slots from the Prophet-6."
                            } else {
                                "Click Add .syx or drop preset files here."
                            },
                        )),
                )
            })
            .when(count != 0, |pane| {
                pane.child(
                    div()
                        .h_9()
                        .flex_none()
                        .flex()
                        .items_center()
                        .px_4()
                        .border_b_1()
                        .border_color(theme.border)
                        .bg(theme.foreground.opacity(0.025))
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.muted_foreground)
                        .child(div().w_8())
                        .child(div().w_16().child("Slot"))
                        .child(div().flex_1().child("Patch"))
                        .child(div().w_48().child("Source")),
                )
                .child(
                    div()
                        .id("patch-list")
                        .track_focus(&self.list_focus)
                        .tab_stop(self.editable())
                        .role(Role::List)
                        .aria_label("Patches")
                        .flex_1()
                        .min_h_0()
                        .overflow_hidden()
                        .on_key_down(cx.listener(Self::list_key))
                        .on_drop(cx.listener(|this, drag: &DragPatches, window, cx| {
                            if let Some(gap) = this.gap_at(window.mouse_position()) {
                                this.reorder(drag.0.clone(), gap, cx);
                            }
                        }))
                        .child(
                            uniform_list("patch-rows", count, move |range, window, cx| {
                                let Some(view) = view.upgrade() else {
                                    return Vec::new();
                                };
                                let this = view.read(cx);
                                let projection = (!this.dragged_ids.is_empty()
                                    && cx.has_active_drag())
                                .then(|| this.gap_at(window.mouse_position()))
                                .flatten()
                                .map(|gap| {
                                    this.collection().projected_order(&this.dragged_ids, gap)
                                });
                                let rows =
                                    projection.as_deref().unwrap_or(&this.collection().patches);
                                range
                                    .map(|i| {
                                        this.row(
                                            i,
                                            &rows[i],
                                            projection.is_some(),
                                            view.downgrade(),
                                            cx,
                                        )
                                    })
                                    .collect()
                            })
                            .size_full()
                            .track_scroll(&self.scroll),
                        ),
                )
                .child(
                    div()
                        .min_h_12()
                        .py_2()
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_between()
                        .px_4()
                        .border_t_1()
                        .border_color(theme.border)
                        .bg(theme.foreground.opacity(0.025))
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_3()
                                        .child(
                                            div()
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(theme.foreground)
                                                .child(format!(
                                                    "{count} / {USER_SLOT_COUNT} patches"
                                                )),
                                        )
                                        .when(count > USER_SLOT_COUNT, |counter| {
                                            counter.child(div().text_color(theme.danger).child(
                                                format!(
                                                    "{} over capacity",
                                                    count - USER_SLOT_COUNT
                                                ),
                                            ))
                                        }),
                                )
                                .when_some(self.sync_status(), |footer, status| {
                                    footer.child(div().child(status))
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .when(selected > 0, |actions| {
                                    actions.child(format!("{selected} selected"))
                                })
                                .child(
                                    Button::new("rename-patch")
                                        .ghost()
                                        .small()
                                        .label("Rename")
                                        .disabled(!self.editable() || selected != 1)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.rename_selected(window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("remove-selected")
                                        .ghost()
                                        .small()
                                        .label("Remove selected")
                                        .tooltip("Remove selected patches from this list (Delete)")
                                        .disabled(!self.editable() || selected == 0)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.remove_selected(window, cx)
                                        })),
                                )
                                .child(div().w_px().h_4().bg(theme.border))
                                .child(
                                    Button::new("clear-list")
                                        .ghost()
                                        .small()
                                        .label("Clear list")
                                        .disabled(!self.editable())
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.clear_list(window, cx)
                                        })),
                                ),
                        ),
                )
            })
    }

    fn gap_at(&self, mouse: Point<Pixels>) -> Option<usize> {
        let scroll = self.scroll.0.borrow();
        let bounds = scroll.base_handle.bounds();
        bounds.contains(&mouse).then(|| {
            let y = mouse.y - bounds.top() - scroll.base_handle.offset().y;
            (f32::from(y) / ROW_HEIGHT).round() as usize
        })
    }

    fn row(
        &self,
        index: usize,
        entry: &Entry,
        projecting: bool,
        view: WeakEntity<Self>,
        cx: &App,
    ) -> Div {
        let id = entry.id;
        let name = entry.patch.name();
        let name: SharedString = if name.is_empty() {
            "Untitled patch".into()
        } else {
            name.into()
        };
        let slot = if index < USER_SLOT_COUNT {
            format!("{index:03}")
        } else {
            "—".into()
        };
        let source = SharedString::from(entry.source.as_ref().to_owned());
        let moving = projecting && self.dragged_ids.contains(&id);
        let selected = self.selected.contains(&id);
        let theme = cx.theme();
        let focus = self.list_focus.clone();
        let click = view.clone();
        div().child(
            ListItem::new(("patch", id))
                .selected(!moving && selected)
                .w_full()
                .h(px(ROW_HEIGHT))
                .rounded_none()
                .px_4()
                .border_b_1()
                .border_color(theme.foreground.opacity(0.05))
                .accessibility_label(format!("Slot {slot}, {name}, {source}"))
                .when(moving, |row| row.bg(theme.primary.opacity(0.09)))
                .when(self.editable() && !projecting, |row| {
                    let drag_focus = focus.clone();
                    let ids = if selected {
                        self.selected.clone()
                    } else {
                        BTreeSet::from([id])
                    };
                    row.on_click(move |event, window, cx| {
                        focus.focus(window, cx);
                        let keys = event.modifiers();
                        let _ = click.update(cx, |this, cx| {
                            this.select(index, keys.shift, keys.platform || keys.control, cx)
                        });
                    })
                    .on_drag(DragPatches(ids), move |drag, _, window, cx| {
                        drag_focus.focus(window, cx);
                        let _ = view.update(cx, |this, cx| {
                            this.dragged_ids = drag.0.clone();
                            cx.notify();
                        });
                        cx.new(|_| DragPreview)
                    })
                })
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .text_sm()
                        .child(
                            div()
                                .w_8()
                                .text_color(if moving {
                                    theme.primary
                                } else {
                                    theme.muted_foreground
                                })
                                .child("⠿"),
                        )
                        .child(
                            div()
                                .w_16()
                                .font_family(theme.mono_font_family.clone())
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(slot),
                        )
                        .child(div().min_w_0().flex_1().pr_3().truncate().child(name))
                        .child(
                            div()
                                .w_48()
                                .flex_none()
                                .truncate()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(source),
                        ),
                ),
        )
    }
}

fn transfer_overlay(transfer: &Transfer, done: usize, theme: &Theme) -> impl IntoElement {
    let (title, label, completed, sending) = match transfer {
        Transfer::Read => (
            "Reading from Prophet-6",
            "Receiving user slots 000–499",
            "received",
            false,
        ),
        Transfer::Send => (
            "Sending to Prophet-6",
            "Replacing user slots 000–499",
            "sent",
            true,
        ),
    };
    div()
        .id("transfer-overlay")
        .absolute()
        .inset_0()
        .occlude()
        .flex()
        .items_center()
        .justify_center()
        .bg(theme.foreground.opacity(0.24))
        .child(
            div()
                .id("transfer-card")
                .role(Role::Dialog)
                .aria_label(title)
                .flex()
                .flex_col()
                .gap_5()
                .p_6()
                .w(px(392.))
                .rounded_2xl()
                .border_1()
                .border_color(theme.border)
                .shadow_lg()
                .bg(theme.background)
                .child(transfer_illustration(sending, theme))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_xl()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(label),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            Progress::new("patch-transfer-progress")
                                .with_size(gpui_kit::component::Size::Small)
                                .value(done as f32 / USER_SLOT_COUNT as f32 * 100.)
                                .color(theme.primary)
                                .accessibility_label(label),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .gap_2()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("{done} of {USER_SLOT_COUNT} {completed}"))
                                .child("Keep USB connected"),
                        ),
                ),
        )
}

pub(crate) fn preview_details(name: &str, theme: &Theme) -> Div {
    let accent = theme.primary;
    let name = if name.is_empty() {
        "Untitled patch"
    } else {
        name
    };
    div()
        .flex()
        .flex_col()
        .gap_3()
        .child(
            div()
                .flex()
                .items_center()
                .gap_4()
                .p_3()
                .rounded_lg()
                .bg(accent.opacity(0.065))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(name.to_owned()),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("After loading, play a key on the synth to hear it."),
                        ),
                )
                .child(synth_panel(theme, Some(4)).flex_none()),
        )
        .child(
            div().text_sm().text_color(theme.muted_foreground).child(
                "Unsaved edits on the synth will be replaced. Saved patches stay unchanged.",
            ),
        )
}

fn synth_panel(theme: &Theme, pressed_key: Option<usize>) -> Div {
    let ink = theme.foreground;
    let accent = theme.primary;
    div()
        .w(px(136.))
        .p_2()
        .rounded_lg()
        .bg(ink)
        .shadow_sm()
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .h_7()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .children((0..5).map(|i| {
                            div()
                                .size(px(9.))
                                .rounded_full()
                                .border_1()
                                .border_color(theme.background.opacity(0.44))
                                .bg(if i == 0 {
                                    accent
                                } else {
                                    theme.background.opacity(0.15)
                                })
                        })),
                )
                .child(
                    div()
                        .text_xs()
                        .font_family(theme.mono_font_family.clone())
                        .text_color(rgb(0xffb49d))
                        .child("06"),
                ),
        )
        .child(
            div()
                .relative()
                .h_6()
                .flex()
                .gap(px(1.))
                .rounded_sm()
                .overflow_hidden()
                .children((0..10).map(|key| {
                    div().flex_1().rounded_sm().bg(if pressed_key == Some(key) {
                        rgb(0xffb49d).into()
                    } else {
                        theme.background
                    })
                }))
                .children([9., 21., 45., 57., 69., 93., 105.].map(|left| {
                    div()
                        .absolute()
                        .top_0()
                        .left(px(left))
                        .w(px(6.))
                        .h(px(14.))
                        .rounded_b_sm()
                        .bg(ink)
                })),
        )
}

fn transfer_illustration(sending: bool, theme: &Theme) -> Div {
    let ink = theme.foreground;
    let accent = theme.primary;
    let patches =
        div()
            .relative()
            .w(px(56.))
            .h(px(64.))
            .children([(8., 0.), (4., 4.), (0., 8.)].map(|(left, top)| {
                div()
                    .absolute()
                    .top(px(top))
                    .left(px(left))
                    .w(px(46.))
                    .h(px(54.))
                    .rounded_md()
                    .border_1()
                    .border_color(accent.opacity(0.38))
                    .bg(theme.background)
                    .flex()
                    .flex_col()
                    .justify_center()
                    .items_start()
                    .gap_1()
                    .px_2()
                    .child(div().w_3().h_1().rounded_full().bg(accent))
                    .child(div().w_6().h(px(2.)).rounded_full().bg(ink.opacity(0.16)))
                    .child(div().w_5().h(px(2.)).rounded_full().bg(ink.opacity(0.16)))
            }));
    div()
        .h(px(124.))
        .rounded_xl()
        .bg(accent.opacity(0.065))
        .flex()
        .items_center()
        .justify_center()
        .gap_3()
        .child(synth_panel(theme, None))
        .child(div().text_color(accent).child(if sending {
            IconName::ArrowLeft
        } else {
            IconName::ArrowRight
        }))
        .child(patches)
}
