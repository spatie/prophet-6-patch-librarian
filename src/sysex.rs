use anyhow::{Context, Result, ensure};

const START: u8 = 0xf0;
const END: u8 = 0xf7;
const PROGRAM_DUMP: [u8; 4] = [START, 0x01, 0x2d, 0x02];
const EDIT_BUFFER_DUMP: [u8; 4] = [START, 0x01, 0x2d, 0x03];
const PACKED_LEN: usize = 1171;
pub(super) const PROGRAM_DUMP_LEN: usize = PROGRAM_DUMP.len() + 2 + PACKED_LEN + 1;
// Zero-based offset and width in the unpacked factory program dumps.
const NAME_OFFSET: usize = 107;
pub(crate) const NAME_LEN: usize = 20;

#[derive(Clone, Debug)]
pub struct Patch {
    pub(crate) slot: Option<u16>,
    packed: Vec<u8>,
}

impl Patch {
    pub fn name(&self) -> String {
        let name: String = (NAME_OFFSET..NAME_OFFSET + NAME_LEN)
            .map(|index| {
                let group = index / 7 * 8;
                let bit = index % 7;
                let byte = self.packed[group + 1 + bit] | ((self.packed[group] >> bit) & 1) << 7;
                match byte {
                    0 => '\0',
                    b' '..=b'~' => char::from(byte),
                    _ => '\u{fffd}',
                }
            })
            .collect();
        name.trim_end_matches([' ', '\0']).replace('\0', "\u{fffd}")
    }

    pub(crate) fn rename(&mut self, name: &str) -> Result<(), &'static str> {
        if name.trim().is_empty() {
            return Err("Enter a patch name.");
        }
        if !name.bytes().all(|byte| (b' '..=b'~').contains(&byte)) {
            return Err("Use unaccented letters, numbers, spaces, or basic punctuation.");
        }
        if name.len() > NAME_LEN {
            return Err("Name is longer than 20 characters.");
        }
        for (offset, index) in (NAME_OFFSET..NAME_OFFSET + NAME_LEN).enumerate() {
            let group = index / 7 * 8;
            let bit = index % 7;
            self.packed[group] &= !(1 << bit);
            self.packed[group + 1 + bit] = name.as_bytes().get(offset).copied().unwrap_or(b' ');
        }
        Ok(())
    }

    pub fn program(&self, slot: u16) -> Result<Vec<u8>> {
        let [bank, program] = address(slot)?;
        Ok(self.dump(&[START, 0x01, 0x2d, PROGRAM_DUMP[3], bank, program]))
    }

    pub fn audition(&self) -> Vec<u8> {
        self.dump(&EDIT_BUFFER_DUMP)
    }

    fn dump(&self, header: &[u8]) -> Vec<u8> {
        let mut message = Vec::with_capacity(header.len() + PACKED_LEN + 1);
        message.extend_from_slice(header);
        message.extend_from_slice(&self.packed);
        message.push(END);
        message
    }
}

pub fn parse(bytes: &[u8]) -> Result<Vec<Patch>> {
    let mut patches = Vec::new();
    let mut offset = 0;
    while offset < bytes.len() {
        ensure!(
            bytes[offset] == START,
            "Expected SysEx start at byte {offset}"
        );
        let data = &bytes[offset + 1..];
        let len = data
            .iter()
            .position(|&byte| byte >= 0x80)
            .with_context(|| format!("Unterminated SysEx frame at byte {offset}"))?;
        ensure!(
            data[len] == END,
            "Unexpected MIDI status byte at byte {}",
            offset + 1 + len
        );
        let end = offset + 1 + len;
        let frame = &bytes[offset..end];
        if let Some(data) = frame.strip_prefix(&PROGRAM_DUMP) {
            let slot = program_dump_slot(frame)
                .with_context(|| format!("Invalid Prophet-6 program address at byte {offset}"))?;
            validate_packed(&data[2..])
                .with_context(|| format!("Invalid Prophet-6 program dump at byte {offset}"))?;
            patches.push(Patch {
                slot: Some(slot),
                packed: data[2..].to_vec(),
            });
        } else if let Some(data) = frame.strip_prefix(&EDIT_BUFFER_DUMP) {
            validate_packed(data)
                .with_context(|| format!("Invalid Prophet-6 edit buffer dump at byte {offset}"))?;
            patches.push(Patch {
                slot: None,
                packed: data.to_vec(),
            });
        }
        offset = end + 1;
    }
    ensure!(
        !patches.is_empty(),
        "No Prophet-6 program or edit buffer dumps found"
    );
    Ok(patches)
}

pub(super) fn program_request(slot: u16) -> Result<[u8; 7]> {
    let [bank, program] = address(slot)?;
    Ok([START, 0x01, 0x2d, 0x05, bank, program, END])
}

pub(super) fn program_dump_slot(message: &[u8]) -> Option<u16> {
    match message.strip_prefix(&PROGRAM_DUMP)? {
        [bank @ 0..=9, program @ 0..=99, ..] => Some(u16::from(*bank) * 100 + u16::from(*program)),
        _ => None,
    }
}

fn address(slot: u16) -> Result<[u8; 2]> {
    ensure!(slot <= 999, "Prophet-6 slot {slot} is outside 000–999");
    Ok([(slot / 100) as u8, (slot % 100) as u8])
}

fn validate_packed(bytes: &[u8]) -> Result<()> {
    ensure!(
        bytes.len() == PACKED_LEN,
        "Expected {PACKED_LEN} packed bytes, got {}",
        bytes.len()
    );
    ensure!(
        bytes.iter().all(|&byte| byte < 0x80),
        "Packed data contains a MIDI status byte"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn factory_name_dump() -> Vec<u8> {
        let mut dump = vec![0xf0, 0x01, 0x2d, 0x02, 0, 0];
        let mut packed = vec![0; PACKED_LEN];
        // P6_Programs_v1.01.syx, slot 000: packed bytes 120..147.
        packed[120..147].copy_from_slice(b"\x00\x05\x00Brass\x00ed Off \x00       \x00 <");
        dump.extend(packed);
        dump.push(0xf7);
        dump
    }

    #[test]
    fn factory_name_and_payload_round_trip() {
        let dump = factory_name_dump();
        let patch = parse(&dump).unwrap().remove(0);
        assert_eq!(patch.slot, Some(0));
        assert_eq!(patch.name(), "Brassed Off");
        assert_eq!(patch.program(0).unwrap(), dump);

        let moved = patch.program(999).unwrap();
        assert_eq!(&moved[..6], &[0xf0, 1, 0x2d, 2, 9, 99]);
        assert_eq!(&moved[6..], &dump[6..]);
        assert_eq!(patch.slot, Some(0));
        assert!(patch.program(1000).is_err());
        assert_eq!(
            program_request(512).unwrap(),
            [0xf0, 1, 0x2d, 5, 5, 12, 0xf7]
        );
    }

    #[test]
    fn audition_frame_matches_the_manual_without_a_program_address() {
        let dump = factory_name_dump();
        let patch = parse(&dump).unwrap().remove(0);
        let edit = patch.audition();
        assert_eq!(edit.len(), 1176);
        assert_eq!(&edit[..4], &[0xf0, 1, 0x2d, 3]);
        assert_eq!(&edit[4..1175], &dump[6..1177]);
        assert_eq!(edit[1175], 0xf7);
    }

    #[test]
    fn renaming_changes_only_the_name_in_each_dump() {
        let mut original = factory_name_dump();
        // The first and last name groups share a high-bit byte with non-name data.
        original[6 + 120] = 0x7f;
        original[6 + 144] = 0x7f;
        let mut patch = parse(&original).unwrap().remove(0);
        let name = "Voltage Keys 1234567";
        assert_eq!(name.len(), 20);
        patch.rename(name).unwrap();
        let program = patch.program(0).unwrap();
        assert_eq!(parse(&program).unwrap()[0].name(), name);
        assert_eq!(parse(&patch.audition()).unwrap()[0].name(), name);
        assert_eq!(&program[..6], &original[..6]);
        assert_eq!(program[6 + 120], 0b0000011);
        assert_eq!(program[6 + 144], 0b1111110);
        assert_eq!(&program[6 + 121..6 + 123], &original[6 + 121..6 + 123]);
        assert_eq!(&program[6 + 146..], &original[6 + 146..]);

        patch.rename("New").unwrap();
        let short = patch.program(0).unwrap();
        assert_eq!(parse(&short).unwrap()[0].name(), "New");
        for invalid in [" ", "Échos", "Line\nbreak", "Voltage Keys 12345678"] {
            assert!(patch.rename(invalid).is_err());
            assert_eq!(patch.program(0).unwrap(), short);
        }
    }

    #[test]
    fn parses_mixed_dumps_in_order() {
        let dump = factory_name_dump();
        let patch = parse(&dump).unwrap().remove(0);
        let bytes = [
            patch.audition(),
            vec![0xf0, 0x7d, 0xf7],
            patch.program(714).unwrap(),
            dump,
        ]
        .concat();
        let patches = parse(&bytes).unwrap();
        assert_eq!(
            patches.iter().map(|p| p.slot).collect::<Vec<_>>(),
            [None, Some(714), Some(0)]
        );
    }

    #[test]
    fn rejects_invalid_dumps() {
        let dump = factory_name_dump();
        assert!(parse(&dump[..dump.len() - 1]).is_err());
        assert!(parse(&[&dump[..dump.len() - 2], &[0xf7]].concat()).is_err());
        assert!(parse(&[0xf0, 0x01, 0x2d, 0x02, 0xf7]).is_err());
        assert!(parse(&[0xf0, 0x7d, 0xf7]).is_err());
        let mut invalid = dump.clone();
        invalid[4] = 10;
        assert!(parse(&invalid).is_err());
        invalid[4] = 0;
        invalid[5] = 100;
        assert!(parse(&invalid).is_err());
        invalid[5] = 0;
        invalid[40] = 0x90;
        assert!(parse(&invalid).is_err());
    }
}
