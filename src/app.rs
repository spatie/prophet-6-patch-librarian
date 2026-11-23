use crate::{
    library::{self, Collection, Entry, USER_SLOT_COUNT},
    midi,
    sysex::{self, Patch},
};
use anyhow::{Context as _, Result, ensure};
use directories::{ProjectDirs, UserDirs};
use gpui_kit::{
    component::{
        ActiveTheme, WindowExt,
        button::ButtonVariant,
        checkbox::Checkbox,
        input::{Input, InputState},
    },
    *,
};
use std::{
    cell::Cell,
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

pub(crate) enum Transfer {
    Read,
    Send,
}

enum SyncState {
    NotRead,
    Read(Vec<u64>),
    Sent(Vec<u64>),
    SendFailed,
}

impl SyncState {
    fn status(&self, patches: &[Entry]) -> Option<&'static str> {
        let matches = |ids: &[u64]| patches.iter().map(|entry| entry.id).eq(ids.iter().copied());
        match self {
            Self::NotRead => Some("Not read from Prophet-6"),
            Self::Read(ids) if matches(ids) => Some("Matches last read"),
            Self::Read(_) => None,
            Self::Sent(ids) if matches(ids) => Some("Sent, not verified"),
            Self::Sent(_) => Some("Working list changed since last send"),
            Self::SendFailed => Some("Send failed; slots may be partially overwritten"),
        }
    }
}

pub(crate) struct Librarian {
    collection: Collection,
    preview_preference: Option<PathBuf>,
    skip_preview_warning: bool,
    sync: SyncState,
    pub(crate) selected: BTreeSet<u64>,
    pub(crate) current: Option<u64>,
    anchor: Option<u64>,
    pub(crate) list_focus: FocusHandle,
    pub(crate) scroll: UniformListScrollHandle,
    pub(crate) dragged_ids: BTreeSet<u64>,
    pub(crate) connected: bool,
    pub(crate) busy: bool,
    pub(crate) status: SharedString,
    pub(crate) error: bool,
    pub(crate) progress: Option<(Transfer, Arc<AtomicUsize>)>,
}

impl Librarian {
    pub(crate) fn new(_: &mut Window, cx: &mut Context<Self>) -> Self {
        let preview_preference = ProjectDirs::from("com", "Spatie", crate::TITLE)
            .map(|dirs| dirs.config_dir().join("skip-preview-warning"));
        let skip_preview_warning = preview_preference
            .as_ref()
            .is_some_and(|path| path.is_file());
        cx.spawn(async move |this, cx| {
            loop {
                let connected = cx.background_spawn(async { midi::connected() }).await;
                if this
                    .update(cx, |this, cx| {
                        let online = connected.as_ref().is_ok_and(|online| *online);
                        if this.connected != online {
                            this.connected = online;
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
                cx.background_executor().timer(Duration::from_secs(2)).await;
            }
        })
        .detach();
        Self {
            collection: Collection::default(),
            preview_preference,
            skip_preview_warning,
            sync: SyncState::NotRead,
            selected: BTreeSet::new(),
            current: None,
            anchor: None,
            list_focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            dragged_ids: BTreeSet::new(),
            connected: false,
            busy: false,
            status: "".into(),
            error: false,
            progress: None,
        }
    }

    pub(crate) fn collection(&self) -> &Collection {
        &self.collection
    }

    pub(crate) fn sync_status(&self) -> Option<&'static str> {
        self.sync.status(&self.collection.patches)
    }

    pub(crate) fn editable(&self) -> bool {
        !self.busy
    }

    fn report(&mut self, status: impl Into<SharedString>, error: bool, cx: &mut Context<Self>) {
        self.status = status.into();
        self.error = error;
        cx.notify();
    }

    fn fail(&mut self, error: anyhow::Error, cx: &mut Context<Self>) {
        self.report(format!("{error:#}"), true, cx);
    }

    pub(crate) fn select(
        &mut self,
        index: usize,
        extend: bool,
        toggle: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.editable() {
            return;
        }
        let entries = &self.collection().patches;
        let id = entries[index].id;
        if extend {
            let anchor = self
                .anchor
                .and_then(|id| entries.iter().position(|e| e.id == id))
                .unwrap_or(index);
            self.selected = entries[anchor.min(index)..=anchor.max(index)]
                .iter()
                .map(|e| e.id)
                .collect();
        } else if toggle {
            if !self.selected.remove(&id) {
                self.selected.insert(id);
            }
            self.anchor = Some(id);
        } else {
            self.selected = BTreeSet::from([id]);
            self.anchor = Some(id);
        }
        self.current = Some(id);
        cx.notify();
    }

    pub(crate) fn reorder(
        &mut self,
        ids: BTreeSet<u64>,
        gap: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.editable() {
            return false;
        }
        if !self.collection.reorder(&ids, gap) {
            return false;
        }
        self.selected = self
            .collection
            .patches
            .iter()
            .map(|entry| entry.id)
            .filter(|id| ids.contains(id))
            .collect();
        self.current = self
            .current
            .filter(|id| self.selected.contains(id))
            .or_else(|| self.selected.first().copied());
        self.anchor = self.current;
        self.report("", false, cx);
        true
    }

    pub(crate) fn list_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.editable() {
            return;
        }
        if event.keystroke.key == "escape" && cx.stop_active_drag(window) {
            cx.stop_propagation();
            return;
        }
        let patches = &self.collection().patches;
        if patches.is_empty() {
            return;
        }
        let position = self
            .current
            .and_then(|id| patches.iter().position(|e| e.id == id));
        let modifiers = event.keystroke.modifiers;
        match event.keystroke.key.as_str() {
            "a" if modifiers.platform || modifiers.control => {
                self.selected = patches.iter().map(|e| e.id).collect();
                cx.notify();
            }
            "backspace" | "delete" if !self.selected.is_empty() => {
                self.remove_selected(window, cx);
            }
            "up" | "down" if modifiers.alt && !self.selected.is_empty() => {
                let up = event.keystroke.key == "up";
                let index = if up {
                    patches
                        .iter()
                        .position(|e| self.selected.contains(&e.id))
                        .unwrap()
                } else {
                    patches
                        .iter()
                        .rposition(|e| self.selected.contains(&e.id))
                        .unwrap()
                };
                if (up && index > 0) || (!up && index + 1 < patches.len()) {
                    self.reorder(
                        self.selected.clone(),
                        if up { index - 1 } else { index + 2 },
                        cx,
                    );
                    if let Some(index) = self
                        .collection()
                        .patches
                        .iter()
                        .position(|e| Some(e.id) == self.current)
                    {
                        self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
                    }
                }
            }
            key @ ("up" | "down" | "home" | "end") => {
                let index = match key {
                    "up" => position.map_or(0, |i| i.saturating_sub(1)),
                    "down" => position.map_or(0, |i| (i + 1).min(patches.len() - 1)),
                    "home" => 0,
                    _ => patches.len() - 1,
                };
                self.select(index, modifiers.shift, false, cx);
                self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(crate) fn rename_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() || self.selected.len() != 1 {
            return;
        }
        let Some(index) = self
            .collection
            .patches
            .iter()
            .position(|e| self.selected.contains(&e.id))
        else {
            return;
        };
        let patch = self.collection.patches[index].patch.clone();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(patch.name()));
        let focus = input.clone();
        let error = Rc::new(Cell::<Option<&str>>::new(None));
        let renamed = Rc::new(Cell::new(None));
        let view = cx.entity().downgrade();
        self.busy = true;
        cx.notify();

        window.open_alert_dialog(cx, move |dialog, _, cx| {
            let theme = cx.theme();
            let hint = error.get();
            let input = input.clone();
            let error = error.clone();
            let on_ok = renamed.clone();
            let on_close = renamed.clone();
            let view = view.clone();
            let patch = patch.clone();
            dialog
                .confirm()
                .centered()
                .title("Rename patch")
                .ok_text("Rename")
                .child(Input::new(&input).aria_label("Patch name"))
                .child(
                    div()
                        .text_sm()
                        .text_color(if hint.is_some() {
                            theme.danger
                        } else {
                            theme.muted_foreground
                        })
                        .child(
                            hint.map(str::to_owned)
                                .unwrap_or_else(|| format!("Up to {} characters", sysex::NAME_LEN)),
                        ),
                )
                .on_ok(move |_, window, cx| {
                    let value = input.read(cx).value();
                    let name = value.trim_end_matches(' ');
                    let mut updated = (*patch).clone();
                    if let Err(message) = updated.rename(name) {
                        error.set(Some(message));
                        window.refresh();
                        return false;
                    }
                    if name != patch.name() {
                        on_ok.set(Some(updated));
                    }
                    true
                })
                .on_close(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.busy = false;
                        if let Some(patch) = on_close.take() {
                            this.collection.replace_patch(index, patch);
                            this.select(index, false, false, cx);
                            this.report("", false, cx);
                        } else {
                            cx.notify();
                        }
                    });
                })
        });
        window.on_next_frame(move |window, cx| {
            focus.update(cx, |state, cx| {
                state.focus(window, cx);
                state.select_all(window, cx);
            });
        });
    }

    pub(crate) fn remove_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() || self.selected.is_empty() {
            return;
        }
        let count = self.selected.len();
        if count == self.collection.patches.len() {
            self.clear_list(window, cx);
            return;
        }
        self.collection.remove(&self.selected);
        self.reset_selection();
        self.list_focus.focus(window, cx);
        self.report(
            format!("Removed {count} {} from the list", patch_word(count)),
            false,
            cx,
        );
    }

    pub(crate) fn clear_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() || self.collection.patches.is_empty() {
            return;
        }
        self.confirm(
            window,
            cx,
            "Clear the working list?",
            "This removes every patch from the list. Your .syx files and the Prophet-6 stay unchanged. Export the list first if you want to keep it.",
            ("Clear list", ButtonVariant::Danger),
            |this, cx| {
                this.collection.patches.clear();
                this.reset_selection();
                this.report("", false, cx);
            },
        );
    }

    fn reset_selection(&mut self) {
        self.selected.clear();
        self.current = None;
        self.anchor = None;
    }

    pub(crate) fn import_picker(&mut self, cx: &mut Context<Self>) {
        if !self.editable() {
            return;
        }
        self.busy = true;
        let answer = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Add .syx files to the list".into()),
        });
        cx.spawn(async move |this, cx| {
            let paths = answer
                .await
                .context("Import dialog closed")
                .and_then(|paths| paths);
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match paths {
                    Ok(Some(paths)) if !paths.is_empty() => this.import(paths, cx),
                    Ok(Some(_)) | Ok(None) => cx.notify(),
                    Err(err) => this.fail(err, cx),
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn import(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if !self.editable() || paths.is_empty() {
            return;
        }
        self.run(
            "Reading SysEx files…",
            None,
            move |_| -> Result<_> {
                let mut packs = Vec::new();
                for path in paths {
                    let patch_file = || sysex::parse(&fs::read(&path)?);
                    let source = path
                        .file_name()
                        .unwrap_or(path.as_os_str())
                        .to_string_lossy();
                    let patches =
                        patch_file().with_context(|| format!("Could not add {source}"))?;
                    packs.push((Arc::<str>::from(source.as_ref()), patches));
                }
                Ok(packs)
            },
            |this, result, cx| match result {
                Ok(packs) => {
                    let start = this.collection.patches.len();
                    for (source, patches) in packs {
                        this.collection.append(patches, source);
                    }
                    let count = this.collection.patches.len() - start;
                    this.scroll
                        .scroll_to_item_strict(start, ScrollStrategy::Top);
                    this.report(
                        format!("Added {count} {} to the list", patch_word(count)),
                        false,
                        cx,
                    );
                }
                Err(err) => this.fail(err, cx),
            },
            cx,
        );
    }

    pub(crate) fn export(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let patches = self.collection.export_patches(&self.selected);
        if patches.is_empty() {
            return;
        }
        if patches.len() > USER_SLOT_COUNT {
            self.report(
                "Select at most 500 patches to export to user slots 000–499.",
                true,
                cx,
            );
            return;
        }
        let directory = UserDirs::new()
            .map(|dirs| dirs.document_dir().unwrap_or(dirs.home_dir()).to_path_buf());
        let Some(directory) = directory else {
            self.report("No home folder is available for export.", true, cx);
            return;
        };
        let answer = cx.prompt_for_new_path(&directory, Some("Prophet-6 Patches.syx"));
        self.busy = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let path = answer
                .await
                .context("Export dialog closed")
                .and_then(|path| path);
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match path {
                    Ok(Some(path)) => {
                        let count = patches.len();
                        let name = path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();
                        this.run(
                            "Exporting patches…",
                            None,
                            move |_| library::export(&path, &patches),
                            move |this, result, cx| match result {
                                Ok(()) => this.report(
                                    format!("Exported {count} {} to {name}", patch_word(count)),
                                    false,
                                    cx,
                                ),
                                Err(err) => this.fail(err, cx),
                            },
                            cx,
                        );
                    }
                    Ok(None) => cx.notify(),
                    Err(err) => this.fail(err, cx),
                }
            });
        })
        .detach();
    }

    pub(crate) fn read(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() || !self.connected {
            return;
        }
        if self.collection.patches.is_empty() {
            self.read_all(cx);
            return;
        }
        self.confirm(
            window,
            cx,
            "Replace the working list?",
            "Reading all 500 user slots replaces the working list. Export it first if you want to keep it.",
            ("Read from Prophet-6", ButtonVariant::Primary),
            Self::read_all,
        );
    }

    fn read_all(&mut self, cx: &mut Context<Self>) {
        self.run(
            "Reading user slots 000–499…",
            Some(Transfer::Read),
            |progress| {
                let slots = (0..USER_SLOT_COUNT as u16).collect::<Vec<_>>();
                let patches = midi::receive(&slots, progress)?;
                ensure!(
                    patches
                        .iter()
                        .map(|patch| patch.slot)
                        .eq(slots.into_iter().map(Some)),
                    "Did not receive all 500 user slots in order"
                );
                Ok(patches)
            },
            |this, result, cx| match result {
                Ok(patches) => {
                    this.collection.patches.clear();
                    this.collection.append(patches, "Prophet-6".into());
                    this.sync =
                        SyncState::Read(this.collection.patches.iter().map(|e| e.id).collect());
                    this.reset_selection();
                    this.scroll.scroll_to_item_strict(0, ScrollStrategy::Top);
                    this.report("", false, cx);
                }
                Err(err) => this.fail(err, cx),
            },
            cx,
        );
    }

    pub(crate) fn audition(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || !self.connected || self.selected.len() != 1 {
            return;
        }
        let Some(entry) = self
            .collection
            .patches
            .iter()
            .find(|e| self.selected.contains(&e.id))
        else {
            return;
        };
        let patch = Arc::clone(&entry.patch);
        if self.skip_preview_warning {
            self.preview(patch, false, cx);
            return;
        }

        let name = patch.name();
        self.busy = true;
        cx.notify();
        let view = cx.entity().downgrade();
        let skip_warning = Rc::new(Cell::new(false));
        let confirmed = Rc::new(Cell::new(false));
        window.open_alert_dialog(cx, move |dialog, _, cx| {
            let view = view.clone();
            let close_skip = skip_warning.clone();
            let change_skip = skip_warning.clone();
            let on_ok = confirmed.clone();
            let on_close = confirmed.clone();
            let patch = patch.clone();
            dialog
                .confirm()
                .centered()
                .title("Preview on Prophet-6")
                .ok_text("Load preview")
                .child(
                    crate::view::preview_details(&name, cx.theme()).child(
                        Checkbox::new("skip-preview-warning")
                            .label("Don’t ask before previewing again")
                            .checked(skip_warning.get())
                            .on_change(move |checked, window, _| {
                                change_skip.set(*checked);
                                window.refresh();
                            }),
                    ),
                )
                .on_ok(move |_, _, _| {
                    on_ok.set(true);
                    true
                })
                .on_close(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| {
                        if on_close.get() {
                            this.preview(patch.clone(), close_skip.get(), cx);
                        } else {
                            this.busy = false;
                            cx.notify();
                        }
                    });
                })
        });
    }

    fn preview(&mut self, patch: Arc<Patch>, skip_warning: bool, cx: &mut Context<Self>) {
        let path = self.preview_preference.clone();
        self.run(
            "Sending preview to the edit buffer…",
            None,
            move |_| {
                let preference = if skip_warning {
                    save_preview_preference(path.as_deref())
                } else {
                    Ok(())
                };
                (midi::audition(&patch), preference)
            },
            move |this, (sent, preference), cx| {
                if skip_warning && preference.is_ok() {
                    this.skip_preview_warning = true;
                }
                let (status, error) = match sent {
                    Ok(()) => ("Preview sent. Play a note on the Prophet-6.".into(), false),
                    Err(err) => (format!("{err:#}"), true),
                };
                match preference {
                    Ok(()) => this.report(status, error, cx),
                    Err(err) => this.report(
                        format!("{status} Could not save the preview preference: {err:#}"),
                        true,
                        cx,
                    ),
                }
            },
            cx,
        );
    }

    pub(crate) fn write(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || !self.connected || self.collection.patches.len() != USER_SLOT_COUNT {
            return;
        }
        self.confirm(
            window,
            cx,
            "Overwrite 500 patches on Prophet-6?",
            "This permanently replaces saved slots 000–499 in the order shown. Export a backup first if you want to keep the current sounds.",
            ("Overwrite 000–499", ButtonVariant::Danger),
            Self::write_all,
        );
    }

    fn confirm(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        title: &'static str,
        description: &'static str,
        (button, variant): (&'static str, ButtonVariant),
        action: fn(&mut Self, &mut Context<Self>),
    ) {
        self.busy = true;
        cx.notify();
        let view = cx.entity().downgrade();
        let confirmed = Rc::new(Cell::new(false));
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let view = view.clone();
            let on_ok = confirmed.clone();
            let on_close = confirmed.clone();
            dialog
                .confirm()
                .close_button(true)
                .title(title)
                .description(description)
                .ok_text(button)
                .ok_variant(variant)
                .on_ok(move |_, _, _| {
                    on_ok.set(true);
                    true
                })
                .on_close(move |_, _, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.busy = false;
                        if on_close.get() {
                            action(this, cx);
                        } else {
                            cx.notify();
                        }
                    });
                })
        });
    }

    fn write_all(&mut self, cx: &mut Context<Self>) {
        let patches = self.collection.export_patches(&BTreeSet::new());
        let ids = self
            .collection
            .patches
            .iter()
            .map(|entry| entry.id)
            .collect();
        self.run(
            "Sending list to slots 000–499…",
            Some(Transfer::Send),
            move |progress| {
                let patches = patches
                    .into_iter()
                    .map(|(slot, patch)| (slot, (*patch).clone()))
                    .collect::<Vec<_>>();
                midi::send(&patches, progress)
            },
            move |this, result, cx| match result {
                Ok(()) => {
                    this.sync = SyncState::Sent(ids);
                    this.report("", false, cx);
                }
                Err(err) => {
                    this.sync = SyncState::SendFailed;
                    this.report(
                        format!("{err:#}. Slots 000–499 may be partially overwritten."),
                        true,
                        cx,
                    );
                }
            },
            cx,
        );
    }

    fn run<T: Send + 'static>(
        &mut self,
        status: &'static str,
        transfer: Option<Transfer>,
        work: impl FnOnce(&mut dyn FnMut(usize)) -> T + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        self.busy = true;
        self.report(status, false, cx);
        let progress = Arc::new(AtomicUsize::new(0));
        if let Some(transfer) = transfer {
            self.progress = Some((transfer, progress.clone()));
            let progress = progress.clone();
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                    if this
                        .update(cx, |this, cx| {
                            let active = this
                                .progress
                                .as_ref()
                                .is_some_and(|(_, counter)| Arc::ptr_eq(counter, &progress));
                            if active {
                                cx.notify();
                            }
                            active
                        })
                        .ok()
                        != Some(true)
                    {
                        break;
                    }
                }
            })
            .detach();
        }
        let task = cx.background_spawn(async move {
            work(&mut |done| progress.store(done, Ordering::Relaxed))
        });
        cx.spawn(async move |this, cx| {
            let value = task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.progress = None;
                done(this, value, cx);
            });
        })
        .detach();
    }
}

fn save_preview_preference(path: Option<&Path>) -> Result<()> {
    let path = path.context("No application config folder")?;
    fs::create_dir_all(path.parent().context("No application config folder")?)?;
    if !path.is_file() {
        fs::File::create_new(path)?;
    }
    Ok(())
}

pub(crate) fn patch_word(count: usize) -> &'static str {
    if count == 1 { "patch" } else { "patches" }
}

#[cfg(test)]
mod tests {
    use super::{Collection, SyncState, sysex};
    use std::collections::BTreeSet;

    #[test]
    fn status_tracks_order_and_preserves_send_failure_after_reorder() {
        let mut frame = vec![0xf0, 1, 0x2d, 3];
        frame.extend([0; 1171]);
        frame.push(0xf7);
        let patch = sysex::parse(&frame).unwrap().remove(0);
        let mut list = Collection::default();
        list.append(vec![patch.clone(), patch], "Prophet-6".into());
        let ids: Vec<_> = list.patches.iter().map(|entry| entry.id).collect();
        let read = SyncState::Read(ids.clone());
        let sent = SyncState::Sent(ids.clone());
        let failed = SyncState::SendFailed;
        assert_eq!(read.status(&list.patches), Some("Matches last read"));
        assert_eq!(sent.status(&list.patches), Some("Sent, not verified"));

        let mut renamed = (*list.patches[0].patch).clone();
        renamed.rename("Keys").unwrap();
        list.replace_patch(0, renamed);
        assert_eq!(read.status(&list.patches), None);
        assert_eq!(
            sent.status(&list.patches),
            Some("Working list changed since last send")
        );

        assert!(list.reorder(&BTreeSet::from([list.patches[0].id]), 2));
        assert_eq!(read.status(&list.patches), None);
        assert_eq!(
            sent.status(&list.patches),
            Some("Working list changed since last send")
        );
        assert_eq!(
            failed.status(&list.patches),
            Some("Send failed; slots may be partially overwritten")
        );
    }
}
