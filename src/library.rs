use crate::sysex::Patch;
use anyhow::{Context, Result};
use std::{collections::BTreeSet, io::Write, path::Path, sync::Arc};

pub(crate) const USER_SLOT_COUNT: usize = 500;

#[derive(Default)]
pub(crate) struct Collection {
    next_id: u64,
    exported_ids: Vec<u64>,
    pub(crate) read_bank: Option<Vec<Arc<Patch>>>,
    pub(crate) patches: Vec<Entry>,
}

#[derive(Clone)]
pub(crate) struct Entry {
    pub(crate) id: u64,
    pub(crate) patch: Arc<Patch>,
}

impl Collection {
    pub(crate) fn needs_export(&self) -> bool {
        !self.patches.is_empty()
            && !self
                .patches
                .iter()
                .map(|entry| entry.id)
                .eq(self.exported_ids.iter().copied())
    }

    pub(crate) fn changed_since_read(&self) -> bool {
        self.read_bank.as_ref().is_some_and(|bank| {
            bank.len() != self.patches.len()
                || self
                    .patches
                    .iter()
                    .zip(bank)
                    .any(|(entry, patch)| !entry.patch.same_program(patch))
        })
    }

    pub(crate) fn mark_read(&mut self) {
        self.read_bank = Some(
            self.patches
                .iter()
                .map(|entry| entry.patch.clone())
                .collect(),
        );
    }

    pub(crate) fn mark_exported(&mut self) {
        self.exported_ids = self.patches.iter().map(|entry| entry.id).collect();
    }

    pub(crate) fn matching_indices(&self, query: &str) -> Vec<usize> {
        let query = query.trim().to_lowercase();
        self.patches
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                (query.is_empty() || entry.patch.name().to_lowercase().contains(&query))
                    .then_some(index)
            })
            .collect()
    }

    pub(crate) fn append(&mut self, patches: Vec<Patch>) {
        self.patches.extend(patches.into_iter().map(|patch| {
            self.next_id += 1;
            Entry {
                id: self.next_id,
                patch: Arc::new(patch),
            }
        }));
    }

    pub(crate) fn remove(&mut self, ids: &BTreeSet<u64>) {
        self.patches.retain(|entry| !ids.contains(&entry.id));
    }

    pub(crate) fn replace_patch(&mut self, index: usize, patch: Patch) {
        // A changed payload must not match the last read or sent list.
        self.next_id += 1;
        let entry = &mut self.patches[index];
        entry.id = self.next_id;
        entry.patch = Arc::new(patch);
    }

    pub(crate) fn projected_order(&self, ids: &BTreeSet<u64>, gap: usize) -> Vec<Entry> {
        let gap = gap.min(self.patches.len());
        let insertion = self.patches[..gap]
            .iter()
            .filter(|entry| !ids.contains(&entry.id))
            .count();
        let (moving, mut stationary): (Vec<_>, Vec<_>) = self
            .patches
            .iter()
            .cloned()
            .partition(|entry| ids.contains(&entry.id));
        stationary.splice(insertion..insertion, moving);
        stationary
    }

    pub(crate) fn reorder(&mut self, ids: &BTreeSet<u64>, gap: usize) -> bool {
        let order = self.projected_order(ids, gap);
        if self
            .patches
            .iter()
            .map(|e| e.id)
            .eq(order.iter().map(|e| e.id))
        {
            return false;
        }
        self.patches = order;
        true
    }

    pub(crate) fn export_patches(&self, selected: &BTreeSet<u64>) -> Vec<(u16, Arc<Patch>)> {
        self.patches
            .iter()
            .filter(|entry| selected.is_empty() || selected.contains(&entry.id))
            .take(USER_SLOT_COUNT + 1)
            .enumerate()
            .map(|(slot, entry)| (slot as u16, Arc::clone(&entry.patch)))
            .collect()
    }
}

pub(crate) fn export(path: &Path, patches: &[(u16, Arc<Patch>)]) -> Result<()> {
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().context("Choose an export folder")?)?;
    for (slot, patch) in patches {
        file.write_all(&patch.program(*slot)?)?;
    }
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sysex;

    fn patch(slot: u8, marker: u8) -> Patch {
        let mut bytes = vec![0xf0, 1, 0x2d, 2, 0, slot, marker];
        bytes.extend([0; 1170]);
        bytes.push(0xf7);
        sysex::parse(&bytes).unwrap().remove(0)
    }

    #[test]
    fn group_drag_previews_without_mutating_then_commits_the_same_order() {
        let mut collection = Collection::default();
        assert!(collection.patches.is_empty());
        collection.append(vec![patch(4, 11), patch(7, 12), patch(2, 13)]);
        collection.append(vec![patch(4, 14), patch(8, 15)]);
        let ids = collection.patches.iter().map(|e| e.id).collect::<Vec<_>>();
        let moving = BTreeSet::from([ids[0], ids[3]]);
        let preview = collection.projected_order(&moving, 3);
        assert_eq!(
            preview.iter().map(|e| e.id).collect::<Vec<_>>(),
            [ids[1], ids[2], ids[0], ids[3], ids[4]]
        );
        assert!(collection.patches.iter().map(|e| e.id).eq(ids));
        assert!(Arc::ptr_eq(&preview[2].patch, &collection.patches[0].patch));
        assert!(collection.reorder(&moving, 3));
        assert!(
            collection
                .patches
                .iter()
                .map(|e| e.id)
                .eq(preview.iter().map(|e| e.id))
        );
        assert_eq!(
            collection
                .patches
                .iter()
                .map(|e| e.patch.slot)
                .collect::<Vec<_>>(),
            [Some(7), Some(2), Some(4), Some(4), Some(8)]
        );
        assert!(!collection.reorder(&moving, 3));
    }

    #[test]
    fn export_preserves_payload_order_and_addresses_the_output_from_zero() {
        let mut collection = Collection::default();
        collection.append(vec![patch(4, 11), patch(7, 12), patch(2, 13)]);
        let selected = BTreeSet::from([collection.patches[2].id, collection.patches[0].id]);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("export.syx");
        for (ids, expected) in [
            (BTreeSet::new(), vec![11, 12, 13]),
            (selected, vec![11, 13]),
        ] {
            export(&path, &collection.export_patches(&ids)).unwrap();
            let patches = sysex::parse(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(patches.len(), expected.len());
            for (slot, (patch, marker)) in patches.iter().zip(expected).enumerate() {
                assert_eq!(patch.slot, Some(slot as u16));
                assert_eq!(patch.audition()[4], marker);
            }
        }
    }

    #[test]
    fn export_reminder_tracks_the_current_list_and_changes_since_read() {
        let mut collection = Collection::default();
        assert!(!collection.needs_export());
        collection.append(vec![patch(4, 11), patch(7, 12), patch(2, 13)]);
        collection.mark_read();
        assert!(collection.needs_export());
        assert!(!collection.changed_since_read());
        collection.mark_exported();
        assert!(!collection.needs_export());

        let first = collection.patches[0].id;
        assert!(collection.reorder(&BTreeSet::from([first]), 3));
        assert!(collection.needs_export());
        assert!(collection.changed_since_read());
        assert!(collection.reorder(&BTreeSet::from([first]), 0));
        assert!(!collection.needs_export());
        assert!(!collection.changed_since_read());

        let mut renamed = (*collection.patches[1].patch).clone();
        renamed.rename("New name").unwrap();
        collection.replace_patch(1, renamed);
        assert!(collection.needs_export());
        assert!(collection.changed_since_read());
        collection.mark_exported();
        assert!(!collection.needs_export());
        assert!(collection.changed_since_read());

        collection.remove(&BTreeSet::from([first]));
        assert!(collection.needs_export());
        collection.mark_exported();
        collection.append(vec![patch(8, 14)]);
        assert!(collection.needs_export());
        collection.patches.clear();
        assert!(!collection.needs_export());
    }

    #[test]
    fn search_matches_names_without_reordering_or_hiding_slot_identity() {
        let named = |slot, marker, name| {
            let mut patch = patch(slot, marker);
            patch.rename(name).unwrap();
            patch
        };
        let mut collection = Collection::default();
        collection.append(vec![named(4, 11, "Warm Pad"), named(7, 12, "Lead")]);
        collection.append(vec![named(8, 14, "Bass"), named(4, 13, "Cold Pad")]);
        collection.append(vec![named(9, 15, "New Pad")]);
        let ids: Vec<_> = collection.patches.iter().map(|entry| entry.id).collect();
        assert_eq!(collection.matching_indices(" PAD "), [0, 3, 4]);
        assert_eq!(collection.matching_indices("lEaD"), [1]);
        assert_eq!(collection.matching_indices("bass"), [2]);
        assert_eq!(collection.matching_indices("CoLd"), [3]);
        assert_eq!(collection.matching_indices(""), [0, 1, 2, 3, 4]);
        assert_eq!(collection.matching_indices(" \t "), [0, 1, 2, 3, 4]);
        assert!(collection.matching_indices("no match").is_empty());
        assert_eq!(
            collection
                .matching_indices("pad")
                .iter()
                .map(|&index| collection.patches[index].patch.slot)
                .collect::<Vec<_>>(),
            [Some(4), Some(4), Some(9)]
        );
        assert!(collection.patches.iter().map(|entry| entry.id).eq(ids));
        assert_eq!(collection.export_patches(&BTreeSet::new()).len(), 5);
    }

    #[test]
    fn removing_from_merged_packs_uses_row_identity_not_original_slot() {
        let mut collection = Collection::default();
        collection.append(vec![patch(4, 11), patch(7, 12)]);
        collection.append(vec![patch(4, 14), patch(8, 15)]);
        let ids = BTreeSet::from([collection.patches[1].id, collection.patches[2].id]);
        collection.remove(&ids);
        let remaining: Vec<_> = collection
            .patches
            .iter()
            .map(|entry| (entry.patch.slot, entry.patch.audition()[4]))
            .collect();
        assert_eq!(remaining, [(Some(4), 11), (Some(8), 15)]);
    }
}
