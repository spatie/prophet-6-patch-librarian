use crate::{
    library::{self, Collection, Entry, USER_SLOT_COUNT},
    midi, session,
    sysex::{self, Patch},
};
use anyhow::{Context as _, Result, ensure};
use directories::{ProjectDirs, UserDirs};
use gpui_kit::{
    component::{
        ActiveTheme, WindowExt,
        button::{Button, ButtonVariant, ButtonVariants},
        checkbox::Checkbox,
        dialog::{Cancel, Confirm},
        input::{InputEvent, InputState},
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

#[derive(Clone, Copy)]
pub(crate) enum CloseIntent {
    Window,
    Quit,
}

impl CloseIntent {
    fn finish(self, window: &mut Window, cx: &mut App) {
        match self {
            Self::Window => window.remove_window(),
            Self::Quit => crate::quit::approved(cx),
        }
    }
}

enum SyncState {
    Unsent,
    Sent(Vec<u64>),
    SendFailed,
}

impl SyncState {
    fn status(&self, patches: &[Entry]) -> Option<&'static str> {
        let matches = |ids: &[u64]| patches.iter().map(|entry| entry.id).eq(ids.iter().copied());
        match self {
            Self::Unsent => None,
            Self::Sent(ids) if matches(ids) => Some("Sent, not verified"),
            Self::Sent(_) => Some("Working list changed since last send"),
            Self::SendFailed => Some("Send failed; slots may be partially overwritten"),
        }
    }
}

pub(crate) struct PatchRename {
    pub(crate) id: u64,
    pub(crate) input: Entity<InputState>,
    pub(crate) error: Option<&'static str>,
    _events: Subscription,
}

pub(crate) struct Librarian {
    collection: Collection,
    pub(crate) search: Entity<InputState>,
    query: String,
    _search_events: Subscription,
    session_path: Option<PathBuf>,
    session_pending: Option<session::Snapshot>,
    session_saving: bool,
    close_after_save: Option<(CloseIntent, AnyWindowHandle)>,
    pub(crate) recovery_status: &'static str,
    preview_preference: Option<PathBuf>,
    skip_preview_warning: bool,
    pub(crate) auto_preview: bool,
    pub(crate) previewing: bool,
    preview_generation: u64,
    pending_preview: Option<u64>,
    last_preview: Option<String>,
    sync: SyncState,
    pub(crate) selected: BTreeSet<u64>,
    pub(crate) current: Option<u64>,
    pub(crate) rename: Option<PatchRename>,
    anchor: Option<u64>,
    pub(crate) list_focus: FocusHandle,
    pub(crate) scroll: UniformListScrollHandle,
    pub(crate) dragged_ids: BTreeSet<u64>,
    pub(crate) connected: bool,
    pub(crate) busy: bool,
    pub(crate) error: Option<SharedString>,
    pub(crate) progress: Option<(Transfer, Arc<AtomicUsize>)>,
}

impl Librarian {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let dirs = ProjectDirs::from("com", "Spatie", crate::TITLE);
        let preview_preference = dirs
            .as_ref()
            .map(|dirs| dirs.config_dir().join("skip-preview-warning"));
        let session_path = dirs.map(|dirs| dirs.data_dir().join("session.p6"));
        let skip_preview_warning = preview_preference
            .as_ref()
            .is_some_and(|path| path.is_file());
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search patches…"));
        let search_events = cx.subscribe(&search, |this, search, event, cx| {
            if matches!(event, InputEvent::Change) {
                this.query = search.read(cx).value().to_string();
                this.filters_changed(cx);
            }
        });
        let path = session_path.clone();
        cx.spawn(async move |this, cx| {
            let restored = cx.background_spawn(async move {
                session::load(&path.context("No application folder is available for session recovery")?)
            }).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match restored {
                    Ok(Some(snapshot)) => {
                        this.collection.append(snapshot.patches.into_iter().map(|patch| (*patch).clone()).collect());
                        if snapshot.exported { this.collection.mark_exported(); }
                        this.collection.read_bank = snapshot.read_bank;
                        this.recovery_status = "Session restored";
                    }
                    Ok(None) => this.recovery_status = "Session recovery ready",
                    Err(error) => {
                        this.recovery_status = "Session recovery unavailable";
                        this.fail(error.context("Could not restore the previous session; its file has been left unchanged"), cx);
                    }
                }
                cx.notify();
            });
        }).detach();
        cx.spawn(async move |this, cx| {
            loop {
                let connected = cx.background_spawn(async { midi::connected() }).await;
                if this
                    .update(cx, |this, cx| {
                        let online = connected.as_ref().is_ok_and(|online| *online);
                        if this.connected != online {
                            this.connected = online;
                            if !online {
                                this.cancel_pending_preview();
                                this.last_preview = None;
                            }
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
            search,
            query: String::new(),
            _search_events: search_events,
            session_path,
            session_pending: None,
            session_saving: false,
            close_after_save: None,
            recovery_status: "Restoring session…",
            preview_preference,
            skip_preview_warning,
            auto_preview: false,
            previewing: false,
            preview_generation: 0,
            pending_preview: None,
            last_preview: None,
            sync: SyncState::Unsent,
            selected: BTreeSet::new(),
            current: None,
            rename: None,
            anchor: None,
            list_focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            dragged_ids: BTreeSet::new(),
            connected: false,
            busy: true,
            error: None,
            progress: None,
        }
    }

    pub(crate) fn collection(&self) -> &Collection {
        &self.collection
    }

    pub(crate) fn visible_indices(&self) -> Vec<usize> {
        self.collection.matching_indices(&self.query)
    }

    pub(crate) fn is_filtered(&self) -> bool {
        !self.query.trim().is_empty()
    }

    pub(crate) fn filters_changed(&mut self, cx: &mut Context<Self>) {
        self.reset_selection();
        self.scroll.scroll_to_item_strict(0, ScrollStrategy::Top);
        cx.notify();
    }

    pub(crate) fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.query.clear();
        self.search
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.filters_changed(cx);
    }

    fn persist_session(&mut self, cx: &mut Context<Self>) {
        self.session_pending = Some(session::Snapshot {
            patches: self
                .collection
                .patches
                .iter()
                .map(|entry| entry.patch.clone())
                .collect(),
            exported: !self.collection.needs_export(),
            read_bank: self.collection.read_bank.clone(),
        });
        self.save_next_session(cx);
    }

    fn save_next_session(&mut self, cx: &mut Context<Self>) {
        if self.session_saving {
            return;
        }
        let Some(snapshot) = self.session_pending.take() else {
            return;
        };
        let Some(path) = self.session_path.clone() else {
            self.close_after_save = None;
            self.busy = false;
            self.fail(
                anyhow::anyhow!("No application folder is available for session recovery"),
                cx,
            );
            return;
        };
        self.session_saving = true;
        self.recovery_status = "Saving session…";
        cx.notify();
        cx.spawn(async move |this, cx| {
            let saved = cx
                .background_spawn(async move { session::save(&path, &snapshot) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.session_saving = false;
                match saved {
                    Ok(()) => {
                        this.recovery_status = "Session saved";
                        if this.session_pending.is_some() {
                            this.save_next_session(cx);
                        } else if let Some((intent, handle)) = this.close_after_save.take() {
                            this.busy = false;
                            cx.defer(move |cx| {
                                let _ =
                                    handle.update(cx, |_, window, cx| intent.finish(window, cx));
                            });
                        }
                    }
                    Err(error) => {
                        this.recovery_status = "Session save failed";
                        if this.close_after_save.take().is_some() {
                            this.busy = false;
                        }
                        this.session_pending = None;
                        this.fail(
                            error.context(
                                "Could not save the session. Export a .syx backup before closing",
                            ),
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn finish_close(
        &mut self,
        intent: CloseIntent,
        handle: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        self.busy = true;
        self.close_after_save = Some((intent, handle));
        self.persist_session(cx);
    }

    pub(crate) fn sync_status(&self) -> Option<&'static str> {
        self.sync.status(&self.collection.patches)
    }

    pub(crate) fn editable(&self) -> bool {
        !self.busy
    }

    pub(crate) fn preview_status(&self) -> String {
        if self.previewing {
            "Loading preview…".into()
        } else if self.auto_preview && !self.connected {
            "Auto preview paused · Connect Prophet-6 by USB".into()
        } else if self.auto_preview && self.selected.len() > 1 {
            "Auto preview paused · Select one patch".into()
        } else if !self.connected {
            "Connect Prophet-6 by USB to preview · Organize and export offline".into()
        } else if let Some(name) = &self.last_preview {
            format!("On Prophet-6: {name}")
        } else if self.auto_preview {
            "Select a patch, then play the Prophet-6".into()
        } else {
            "Preview sounds without overwriting saved patches".into()
        }
    }

    fn fail(&mut self, error: anyhow::Error, cx: &mut Context<Self>) {
        self.error = Some(format!("{error:#}").into());
        cx.notify();
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
        let changed = self.selected.len() != 1 || !self.selected.contains(&id);
        if extend {
            let visible = self.visible_indices();
            let position = visible.iter().position(|i| *i == index).unwrap();
            let anchor = self
                .anchor
                .and_then(|id| visible.iter().position(|i| entries[*i].id == id))
                .unwrap_or(position);
            self.selected = visible[anchor.min(position)..=anchor.max(position)]
                .iter()
                .map(|i| entries[*i].id)
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
        if changed || extend || toggle {
            self.cancel_pending_preview();
        }
        if changed && !extend && !toggle {
            self.schedule_preview(cx);
        }
        cx.notify();
    }

    pub(crate) fn reorder(
        &mut self,
        ids: BTreeSet<u64>,
        gap: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.editable() || self.is_filtered() {
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
        self.error = None;
        self.persist_session(cx);
        cx.notify();
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
        let visible = self.visible_indices();
        let patches = &self.collection().patches;
        if visible.is_empty() {
            return;
        }
        let position = self
            .current
            .and_then(|id| visible.iter().position(|i| patches[*i].id == id));
        let modifiers = event.keystroke.modifiers;
        if self.rename.is_some() {
            return;
        }
        match event.keystroke.key.as_str() {
            "enter" if self.selected.len() == 1 && !modifiers.modified() => {
                if let Some(position) =
                    position.filter(|i| self.selected.contains(&patches[visible[*i]].id))
                {
                    self.start_rename(visible[position], window, cx);
                }
            }
            "a" if modifiers.platform || modifiers.control => {
                self.selected = visible.iter().map(|i| patches[*i].id).collect();
                self.cancel_pending_preview();
                cx.notify();
            }
            "backspace" | "delete" if !self.selected.is_empty() => {
                self.remove_selected(window, cx);
            }
            "up" | "down" if modifiers.alt && !self.selected.is_empty() => {
                if self.is_filtered() {
                    return;
                }
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
                    "down" => position.map_or(0, |i| (i + 1).min(visible.len() - 1)),
                    "home" => 0,
                    _ => visible.len() - 1,
                };
                self.select(visible[index], modifiers.shift, false, cx);
                self.scroll.scroll_to_item(index, ScrollStrategy::Nearest);
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    pub(crate) fn start_rename(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.editable() {
            return;
        }
        let entry = &self.collection.patches[index];
        let id = entry.id;
        let name = entry.patch.name();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        let events = cx.subscribe_in(
            &input,
            window,
            |this, input, event, window, cx| match event {
                InputEvent::PressEnter { .. } => this.commit_rename(window, cx),
                InputEvent::Blur => this.cancel_rename(cx),
                InputEvent::Change => {
                    if let Some(rename) = &mut this.rename
                        && rename.input == *input
                        && rename.error.take().is_some()
                    {
                        cx.notify();
                    }
                }
                InputEvent::Focus => {}
            },
        );
        self.select(index, false, false, cx);
        if let Some(position) = self.visible_indices().iter().position(|i| *i == index) {
            self.scroll
                .scroll_to_item(position, ScrollStrategy::Nearest);
        }
        self.rename = Some(PatchRename {
            id,
            input: input.clone(),
            error: None,
            _events: events,
        });
        window.on_next_frame(move |window, cx| {
            input.update(cx, |state, cx| {
                state.focus(window, cx);
                state.select_all(window, cx);
            });
        });
    }

    fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = self.rename.as_mut() else {
            return;
        };
        let Some(index) = self
            .collection
            .patches
            .iter()
            .position(|entry| entry.id == rename.id)
        else {
            self.cancel_rename(cx);
            return;
        };
        let value = rename.input.read(cx).value();
        let name = value.trim_end_matches(' ');
        let patch = &self.collection.patches[index].patch;
        if name != patch.name() {
            let mut updated = (**patch).clone();
            if let Err(error) = updated.rename(name) {
                rename.error = Some(error);
                cx.notify();
                return;
            }
            self.collection.replace_patch(index, updated);
            if self.visible_indices().contains(&index) {
                self.select(index, false, false, cx);
            } else {
                self.reset_selection();
            }
            self.persist_session(cx);
        }
        self.cancel_rename(cx);
        self.list_focus.focus(window, cx);
    }

    pub(crate) fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        if self.rename.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn remove_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() || self.selected.is_empty() {
            return;
        }
        if self.selected.len() == self.collection.patches.len() {
            self.clear_list(window, cx);
            return;
        }
        self.collection.remove(&self.selected);
        self.reset_selection();
        self.list_focus.focus(window, cx);
        self.error = None;
        self.persist_session(cx);
        cx.notify();
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
                this.error = None;
                this.persist_session(cx);
                cx.notify();
            },
        );
    }

    fn reset_selection(&mut self) {
        self.cancel_pending_preview();
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
            None,
            move |_| -> Result<_> {
                let mut patches = Vec::new();
                for path in paths {
                    let patch_file = || sysex::parse(&fs::read(&path)?);
                    let filename = path
                        .file_name()
                        .unwrap_or(path.as_os_str())
                        .to_string_lossy();
                    patches
                        .extend(patch_file().with_context(|| format!("Could not add {filename}"))?);
                }
                Ok(patches)
            },
            |this, result, cx| match result {
                Ok(patches) => {
                    let start = this.collection.patches.len();
                    this.collection.append(patches);
                    this.persist_session(cx);
                    this.scroll.scroll_to_item_strict(
                        if this.is_filtered() { 0 } else { start },
                        ScrollStrategy::Top,
                    );
                    cx.notify();
                }
                Err(err) => this.fail(err, cx),
            },
            cx,
        );
    }

    pub(crate) fn export(&mut self, cx: &mut Context<Self>) {
        self.export_with_close(None, cx);
    }

    fn export_with_close(
        &mut self,
        close: Option<(CloseIntent, AnyWindowHandle)>,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.cancel_pending_preview();
        let all = BTreeSet::new();
        let patches = self.collection.export_patches(if close.is_some() {
            &all
        } else {
            &self.selected
        });
        let full_list = patches.len() == self.collection.patches.len();
        if patches.is_empty() {
            return;
        }
        if patches.len() > USER_SLOT_COUNT {
            self.fail(
                anyhow::anyhow!("The list has more than 500 patches. Export selections of at most 500 patches before closing."),
                cx,
            );
            return;
        }
        let directory = UserDirs::new()
            .map(|dirs| dirs.document_dir().unwrap_or(dirs.home_dir()).to_path_buf());
        let Some(directory) = directory else {
            self.fail(
                anyhow::anyhow!("No home folder is available for export."),
                cx,
            );
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
                        this.run(
                            None,
                            move |_| library::export(&path, &patches),
                            move |this, result, cx| match result {
                                Ok(()) => {
                                    if full_list {
                                        this.collection.mark_exported();
                                    }
                                    cx.notify();
                                    if let Some((intent, handle)) = close {
                                        this.finish_close(intent, handle, cx);
                                    } else {
                                        this.persist_session(cx);
                                    }
                                }
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

    pub(crate) fn request_close(
        &mut self,
        intent: CloseIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.previewing {
            return;
        }
        self.cancel_pending_preview();
        self.cancel_rename(cx);
        if !self.collection.needs_export() {
            self.finish_close(intent, window.window_handle(), cx);
            return;
        }
        self.busy = true;
        cx.notify();
        let description = if self.collection.changed_since_read() {
            "You’ve changed the list since reading from the Prophet-6. Export the whole list to keep this order and your edits."
        } else {
            "This working list hasn’t been exported. Save a .syx file to keep these patches in the order shown."
        };
        let view = cx.entity().downgrade();
        let confirmed = Rc::new(Cell::new(false));
        let export = Rc::new(Cell::new(true));
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let on_ok = confirmed.clone();
            let on_close = confirmed.clone();
            let discard = export.clone();
            let export = export.clone();
            let view = view.clone();
            dialog
                .confirm()
                .centered()
                .width(px(488.))
                .close_button(false)
                .title("Export before closing?")
                .description(description)
                .footer(
                    div()
                        .w_full()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            Button::new("close-without-export")
                                .ghost()
                                .label("Don’t export")
                                .on_click(move |_, window, cx| {
                                    discard.set(false);
                                    window.dispatch_action(
                                        Box::new(Confirm { secondary: false }),
                                        cx,
                                    );
                                }),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new("cancel-close").label("Cancel").on_click(
                                |_, window, cx| window.dispatch_action(Box::new(Cancel), cx),
                            ),
                        )
                        .child(
                            Button::new("export-and-close")
                                .primary()
                                .label("Export list…")
                                .on_click(|_, window, cx| {
                                    window
                                        .dispatch_action(Box::new(Confirm { secondary: false }), cx)
                                }),
                        ),
                )
                .on_ok(move |_, _, _| {
                    on_ok.set(true);
                    true
                })
                .on_close(move |_, window, cx| {
                    let _ = view.update(cx, |this, cx| {
                        this.busy = false;
                        if on_close.get() {
                            if export.get() {
                                this.export_with_close(Some((intent, window.window_handle())), cx);
                            } else {
                                this.finish_close(intent, window.window_handle(), cx);
                            }
                        }
                        cx.notify();
                    });
                })
        });
    }

    pub(crate) fn read(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.editable() || self.previewing || !self.connected {
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
                    this.collection.append(patches);
                    this.collection.mark_read();
                    this.sync = SyncState::Unsent;
                    this.last_preview = None;
                    this.reset_selection();
                    this.scroll.scroll_to_item_strict(0, ScrollStrategy::Top);
                    this.persist_session(cx);
                    cx.notify();
                }
                Err(err) => this.fail(err, cx),
            },
            cx,
        );
    }

    pub(crate) fn audition(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.previewing || !self.connected || self.selected.len() != 1 {
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
        self.cancel_pending_preview();
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
                        this.busy = false;
                        if on_close.get() {
                            this.preview(patch.clone(), close_skip.get(), cx);
                        } else {
                            cx.notify();
                        }
                    });
                })
        });
    }

    pub(crate) fn toggle_auto_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.cancel_pending_preview();
        if self.auto_preview {
            self.auto_preview = false;
            cx.notify();
            return;
        }
        self.confirm(
            window,
            cx,
            "Preview patches as you browse?",
            "Selecting a patch or using ↑ / ↓ will load it into the Prophet-6 edit buffer. Play the synth to hear it. Unsaved edits on the synth will be replaced; saved patches stay unchanged.",
            ("Enable auto preview", ButtonVariant::Primary),
            |this, cx| {
                this.auto_preview = true;
                this.schedule_preview(cx);
                cx.notify();
            },
        );
    }

    fn cancel_pending_preview(&mut self) {
        self.preview_generation += 1;
        self.pending_preview = None;
    }

    fn schedule_preview(&mut self, cx: &mut Context<Self>) {
        if !self.auto_preview || !self.connected || self.busy || self.selected.len() != 1 {
            return;
        }
        let generation = self.preview_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(120))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.preview_generation == generation {
                    this.preview_selected(cx);
                }
            });
        })
        .detach();
    }

    fn preview_selected(&mut self, cx: &mut Context<Self>) {
        if !self.auto_preview || !self.connected || self.busy || self.selected.len() != 1 {
            return;
        }
        let id = *self.selected.first().unwrap();
        if self.previewing {
            self.pending_preview = Some(id);
            return;
        }
        if let Some(entry) = self.collection.patches.iter().find(|entry| entry.id == id) {
            self.preview(entry.patch.clone(), false, cx);
        }
    }

    fn preview(&mut self, patch: Arc<Patch>, skip_warning: bool, cx: &mut Context<Self>) {
        let path = self.preview_preference.clone();
        let name = patch.name();
        self.previewing = true;
        self.error = None;
        cx.notify();
        let task = cx.background_spawn(async move {
            let preference = if skip_warning {
                save_preview_preference(path.as_deref())
            } else {
                Ok(())
            };
            let sent = midi::audition(&patch);
            // Let the synth process its edit buffer before sending the next selection.
            std::thread::sleep(Duration::from_millis(100));
            (sent, preference)
        });
        cx.spawn(async move |this, cx| {
            let (sent, preference) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.previewing = false;
                if sent.is_ok() && this.connected {
                    this.last_preview = Some(if name.is_empty() {
                        "Untitled patch".into()
                    } else {
                        name
                    });
                }
                if sent.is_err() {
                    this.auto_preview = false;
                    this.cancel_pending_preview();
                    this.last_preview = None;
                }
                if skip_warning && preference.is_ok() {
                    this.skip_preview_warning = true;
                }
                let preference = preference.context("Could not save the preview preference");
                match (sent, preference) {
                    (Ok(()), Ok(())) => cx.notify(),
                    (Err(error), Ok(())) | (Ok(()), Err(error)) => this.fail(error, cx),
                    (Err(send), Err(preference)) => {
                        this.fail(anyhow::anyhow!("{send:#}. {preference:#}"), cx);
                    }
                }
                if this
                    .pending_preview
                    .take()
                    .is_some_and(|id| this.selected.contains(&id))
                {
                    this.preview_selected(cx);
                }
            });
        })
        .detach();
    }

    pub(crate) fn write(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy
            || self.previewing
            || !self.connected
            || self.collection.patches.len() != USER_SLOT_COUNT
        {
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
        self.cancel_pending_preview();
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
                    cx.notify();
                }
                Err(err) => {
                    this.sync = SyncState::SendFailed;
                    this.fail(
                        anyhow::anyhow!("{err:#}. Slots 000–499 may be partially overwritten."),
                        cx,
                    );
                }
            },
            cx,
        );
    }

    fn run<T: Send + 'static>(
        &mut self,
        transfer: Option<Transfer>,
        work: impl FnOnce(&mut dyn FnMut(usize)) -> T + Send + 'static,
        done: impl FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        self.cancel_pending_preview();
        self.busy = true;
        self.error = None;
        cx.notify();
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
        list.append(vec![patch.clone(), patch]);
        let ids: Vec<_> = list.patches.iter().map(|entry| entry.id).collect();
        let sent = SyncState::Sent(ids.clone());
        let failed = SyncState::SendFailed;
        assert_eq!(SyncState::Unsent.status(&list.patches), None);
        assert_eq!(sent.status(&list.patches), Some("Sent, not verified"));

        let mut renamed = (*list.patches[0].patch).clone();
        renamed.rename("Keys").unwrap();
        list.replace_patch(0, renamed);
        assert_eq!(
            sent.status(&list.patches),
            Some("Working list changed since last send")
        );

        assert!(list.reorder(&BTreeSet::from([list.patches[0].id]), 2));
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
