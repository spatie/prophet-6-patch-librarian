use crate::sysex::{self, Patch};
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    io::{ErrorKind, Read, Write},
    path::Path,
    sync::Arc,
};

// Version 1 uses little-endian u32 counts and byte lengths, with complete
// SysEx frames so program addresses and edit buffers stay intact.
const HEADER: &[u8] = b"P6LS\x01";

#[derive(Clone)]
pub(crate) struct Snapshot {
    pub(crate) patches: Vec<Arc<Patch>>,
    pub(crate) exported: bool,
    pub(crate) read_bank: Option<Vec<Arc<Patch>>>,
}

pub(crate) fn load(path: &Path) -> Result<Option<Snapshot>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("Could not read saved session"),
    };
    ensure!(
        bytes.starts_with(HEADER),
        "Unknown session format or version"
    );
    let mut input = &bytes[HEADER.len()..];
    let exported = read_flag(&mut input)?;
    let mut patches = Vec::new();
    for _ in 0..read_len(&mut input)? {
        patches.push(read_patch(&mut input)?);
    }
    let read_bank = if read_flag(&mut input)? {
        let mut patches = Vec::new();
        for _ in 0..read_len(&mut input)? {
            patches.push(read_patch(&mut input)?);
        }
        Some(patches)
    } else {
        None
    };
    ensure!(input.is_empty(), "Unexpected data after saved session");
    Ok(Some(Snapshot {
        patches,
        exported,
        read_bank,
    }))
}

pub(crate) fn save(path: &Path, snapshot: &Snapshot) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).context("Could not create session folder")?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(HEADER)?;
    file.write_all(&[u8::from(snapshot.exported)])?;
    write_len(&mut file, snapshot.patches.len())?;
    for patch in &snapshot.patches {
        write_bytes(&mut file, &patch_bytes(patch)?)?;
    }
    file.write_all(&[u8::from(snapshot.read_bank.is_some())])?;
    if let Some(patches) = &snapshot.read_bank {
        write_len(&mut file, patches.len())?;
        for patch in patches {
            write_bytes(&mut file, &patch_bytes(patch)?)?;
        }
    }
    file.as_file().sync_all()?;
    file.persist(path)
        .context("Could not replace saved session")?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(())
}

fn patch_bytes(patch: &Patch) -> Result<Vec<u8>> {
    match patch.slot {
        Some(slot) => patch.program(slot),
        None => Ok(patch.audition()),
    }
}

fn read_patch(input: &mut &[u8]) -> Result<Arc<Patch>> {
    let bytes = read_bytes(input)?;
    let mut patches = sysex::parse(bytes).context("Invalid patch in saved session")?;
    ensure!(patches.len() == 1, "Expected one patch per session entry");
    let patch = patches.remove(0);
    ensure!(
        patch_bytes(&patch)? == bytes,
        "Unexpected SysEx data in session entry"
    );
    Ok(Arc::new(patch))
}

fn read_flag(input: &mut &[u8]) -> Result<bool> {
    let mut value = [0];
    input.read_exact(&mut value)?;
    ensure!(value[0] <= 1, "Invalid flag in saved session");
    Ok(value[0] == 1)
}

fn read_len(input: &mut &[u8]) -> Result<usize> {
    let mut bytes = [0; 4];
    input.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes) as usize)
}

fn read_bytes<'a>(input: &mut &'a [u8]) -> Result<&'a [u8]> {
    let len = read_len(input)?;
    ensure!(len <= input.len(), "Truncated saved session field");
    let (bytes, rest) = input.split_at(len);
    *input = rest;
    Ok(bytes)
}

fn write_len(output: &mut impl Write, len: usize) -> Result<()> {
    let len = u32::try_from(len).context("Session field exceeds format size limit")?;
    output.write_all(&len.to_le_bytes())?;
    Ok(())
}

fn write_bytes(output: &mut impl Write, bytes: &[u8]) -> Result<()> {
    write_len(output, bytes.len())?;
    output.write_all(bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(slot: Option<u16>, marker: u8) -> Arc<Patch> {
        let mut bytes = vec![0xf0, 1, 0x2d, 3];
        bytes.extend([marker; 1171]);
        bytes.push(0xf7);
        let mut patch = sysex::parse(&bytes).unwrap().remove(0);
        patch.slot = slot;
        Arc::new(patch)
    }

    fn assert_snapshot(actual: &Snapshot, expected: &Snapshot) {
        assert_eq!(actual.exported, expected.exported);
        assert_eq!(actual.patches.len(), expected.patches.len());
        for (actual, expected) in actual.patches.iter().zip(&expected.patches) {
            assert_eq!(actual.slot, expected.slot);
            assert_eq!(patch_bytes(actual).unwrap(), patch_bytes(expected).unwrap());
        }
        match (&actual.read_bank, &expected.read_bank) {
            (None, None) => {}
            (Some(patches), Some(expected_patches)) => {
                assert_eq!(patches.len(), expected_patches.len());
                for (actual, expected) in patches.iter().zip(expected_patches) {
                    assert_eq!(actual.slot, expected.slot);
                    assert_eq!(patch_bytes(actual).unwrap(), patch_bytes(expected).unwrap());
                }
            }
            _ => panic!("Last synth read was not preserved"),
        }
    }

    #[test]
    fn round_trip_preserves_order_frames_exported_and_read_bank() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("nested/session.bin");
        let snapshot = Snapshot {
            patches: vec![
                patch(Some(999), 11),
                patch(None, 22),
                patch(Some(0), 33),
                patch(Some(714), 44),
            ],
            exported: false,
            read_bank: Some(vec![patch(Some(421), 55), patch(None, 66)]),
        };
        for exported in [false, true] {
            let snapshot = Snapshot {
                exported,
                ..snapshot.clone()
            };
            save(&path, &snapshot).unwrap();
            assert_snapshot(&load(&path).unwrap().unwrap(), &snapshot);
        }
    }

    #[test]
    fn round_trip_has_no_500_patch_limit_for_list_or_read_bank() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.bin");
        let patches: Vec<_> = (0..1203)
            .map(|index| {
                patch(
                    (index % 3 != 0).then_some((index % 1000) as u16),
                    (index % 128) as u8,
                )
            })
            .collect();
        let snapshot = Snapshot {
            read_bank: Some(patches.iter().rev().cloned().collect()),
            patches,
            exported: true,
        };
        save(&path, &snapshot).unwrap();
        assert_snapshot(&load(&path).unwrap().unwrap(), &snapshot);
    }

    #[test]
    fn empty_lists_and_optional_empty_read_bank_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.bin");
        for read_bank in [None, Some(Vec::new())] {
            let snapshot = Snapshot {
                patches: Vec::new(),
                exported: false,
                read_bank,
            };
            save(&path, &snapshot).unwrap();
            assert_snapshot(&load(&path).unwrap().unwrap(), &snapshot);
            let expected = if snapshot.read_bank.is_some() {
                b"P6LS\x01\x00\x00\x00\x00\x00\x01\x00\x00\x00\x00".as_slice()
            } else {
                b"P6LS\x01\x00\x00\x00\x00\x00\x00".as_slice()
            };
            assert_eq!(fs::read(&path).unwrap(), expected);
        }
    }

    #[test]
    fn missing_is_none_but_io_errors_corruption_and_every_truncation_are_errors() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.bin");
        assert!(load(&path).unwrap().is_none());
        assert!(load(directory.path()).is_err());
        let snapshot = Snapshot {
            patches: vec![patch(Some(321), 12)],
            exported: true,
            read_bank: Some(vec![patch(Some(0), 56)]),
        };
        save(&path, &snapshot).unwrap();
        let bytes = fs::read(&path).unwrap();
        for len in 0..bytes.len() {
            fs::write(&path, &bytes[..len]).unwrap();
            assert!(load(&path).is_err(), "Accepted truncation at byte {len}");
        }
        let mut corruptions = vec![b"not a session".to_vec()];
        for (offset, value) in [
            (0, b'X'),
            (HEADER.len() - 1, 2),
            (HEADER.len(), 2),
            (HEADER.len() + 1 + 4 + 4, 0xff),
            (bytes.len() - 1, 0),
        ] {
            let mut corrupt = bytes.clone();
            corrupt[offset] = value;
            corruptions.push(corrupt);
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        corruptions.push(trailing);
        let mut oversized = bytes.clone();
        oversized[HEADER.len() + 1..HEADER.len() + 5].fill(0xff);
        corruptions.push(oversized);
        for corrupt in corruptions {
            fs::write(&path, corrupt).unwrap();
            assert!(load(&path).is_err());
        }
    }

    #[test]
    fn rejects_extra_sysex_frames_in_a_single_patch_entry() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.bin");
        for extra in [
            vec![0xf0, 0x7d, 0xf7],
            patch_bytes(&patch(None, 2)).unwrap(),
        ] {
            let mut bytes = HEADER.to_vec();
            bytes.push(0);
            write_len(&mut bytes, 1).unwrap();
            let mut frame = patch_bytes(&patch(Some(42), 1)).unwrap();
            frame.extend(extra);
            write_bytes(&mut bytes, &frame).unwrap();
            bytes.push(0);
            fs::write(&path, bytes).unwrap();
            assert!(load(&path).is_err());
        }
    }

    #[test]
    fn invalid_serialization_preserves_previous_file_and_removes_tempfile() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.bin");
        let original = Snapshot {
            patches: vec![patch(Some(42), 12)],
            exported: true,
            read_bank: None,
        };
        save(&path, &original).unwrap();
        let previous = fs::read(&path).unwrap();
        let mut invalid = original.clone();
        invalid.patches.push(patch(Some(1000), 23));
        assert!(save(&path, &invalid).is_err());
        assert_eq!(fs::read(&path).unwrap(), previous);
        assert_snapshot(&load(&path).unwrap().unwrap(), &original);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);

        invalid.patches.pop();
        invalid.read_bank = Some(vec![patch(Some(1000), 56)]);
        assert!(save(&path, &invalid).is_err());
        assert_eq!(fs::read(&path).unwrap(), previous);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);

        invalid.read_bank = Some(vec![patch(Some(0), 67)]);
        invalid.exported = false;
        save(&path, &invalid).unwrap();
        assert_snapshot(&load(&path).unwrap().unwrap(), &invalid);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
