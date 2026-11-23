use crate::sysex::{self, Patch};
use anyhow::{Context, Result, anyhow, bail, ensure};
#[cfg(not(target_os = "macos"))]
use midir::MidiIO;
use midir::{Ignore, MidiInput, MidiOutput, MidiOutputConnection, MidiOutputPort};
use std::{
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::{Duration, Instant},
};

const CLIENT_NAME: &str = "Patch Librarian";
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);
// The unit does not acknowledge writes; leave time for it to process each dump.
const WRITE_INTERVAL: Duration = Duration::from_millis(100);

pub fn connected() -> Result<bool> {
    #[cfg(target_os = "macos")]
    {
        Ok(macos::prophet_ports()?.is_some())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let input = MidiInput::new(CLIENT_NAME).context("Could not initialize MIDI input")?;
        let output = MidiOutput::new(CLIENT_NAME).context("Could not initialize MIDI output")?;
        let input = prophet_port(&input, "input")?;
        let output = prophet_port(&output, "output")?;
        Ok(input.is_some() && output.is_some())
    }
}

pub fn receive(slots: &[u16], mut progress: impl FnMut(usize)) -> Result<Vec<Patch>> {
    let requests = slots
        .iter()
        .copied()
        .map(sysex::program_request)
        .collect::<Result<Vec<_>>>()?;
    if requests.is_empty() {
        return Ok(Vec::new());
    }

    let (tx, rx) = mpsc::channel();
    let mut input = MidiInput::new(CLIENT_NAME).context("Could not initialize MIDI input")?;
    input.ignore(Ignore::TimeAndActiveSense);
    #[cfg(target_os = "macos")]
    let (port, mut output) = {
        let (source, destination) = macos::prophet_ports()?.context(macos::NO_PORTS)?;
        let port = input
            .find_port_by_id(source.to_string())
            .context(macos::NO_PORTS)?;
        (port, macos::open_output(destination)?)
    };
    #[cfg(not(target_os = "macos"))]
    let port = prophet_port(&input, "input")?
        .context("No Prophet-6 MIDI input found. Connect it by USB.")?;
    let _input = input
        .connect(
            &port,
            CLIENT_NAME,
            move |_, message, rx_fragment| {
                if let Some(dump) = collect_program_dump(rx_fragment, message) {
                    let _ = tx.send(dump);
                }
            },
            None,
        )
        .map_err(|err| anyhow!("Could not open Prophet-6 MIDI input: {err}"))?;
    #[cfg(not(target_os = "macos"))]
    let mut output = open_output()?;

    let mut patches = Vec::with_capacity(slots.len());
    for (&slot, request) in slots.iter().zip(requests) {
        let requested_at = Instant::now();
        output
            .send(&request)
            .with_context(|| format!("Could not request Prophet-6 slot {slot:03}"))?;
        patches.push(wait_for_program(&rx, slot, requested_at, RESPONSE_TIMEOUT)?);
        progress(patches.len());
    }
    Ok(patches)
}

pub fn send(patches: &[(u16, Patch)], mut progress: impl FnMut(usize)) -> Result<()> {
    let writes = program_writes(patches)?;
    if writes.is_empty() {
        return Ok(());
    }

    let mut output = open_output()?;
    for (index, (&(slot, _), message)) in patches.iter().zip(writes).enumerate() {
        if index != 0 {
            thread::sleep(WRITE_INTERVAL);
        }
        output
            .send(&message)
            .with_context(|| format!("Could not send Prophet-6 slot {slot:03}"))?;
        progress(index + 1);
    }
    Ok(())
}

pub fn audition(patch: &Patch) -> Result<()> {
    open_output()?
        .send(&patch.audition())
        .context("Could not send the Prophet-6 edit buffer")
}

fn program_writes(patches: &[(u16, Patch)]) -> Result<Vec<Vec<u8>>> {
    patches
        .iter()
        .map(|(slot, patch)| {
            ensure!(
                *slot < 500,
                "Cannot write Prophet-6 slot {slot:03}; only user slots 000–499 are writable"
            );
            patch.program(*slot)
        })
        .collect()
}

fn open_output() -> Result<MidiOutputConnection> {
    #[cfg(target_os = "macos")]
    {
        let (_, destination) = macos::prophet_ports()?.context(macos::NO_PORTS)?;
        macos::open_output(destination)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let output = MidiOutput::new(CLIENT_NAME).context("Could not initialize MIDI output")?;
        let port = prophet_port(&output, "output")?
            .context("No Prophet-6 MIDI output found. Connect it by USB.")?;
        connect_output(output, &port)
    }
}

fn connect_output(output: MidiOutput, port: &MidiOutputPort) -> Result<MidiOutputConnection> {
    output
        .connect(port, CLIENT_NAME)
        .map_err(|err| anyhow!("Could not open Prophet-6 MIDI output: {err}"))
}

#[cfg(not(target_os = "macos"))]
fn prophet_port<M: MidiIO>(midi: &M, direction: &str) -> Result<Option<M::Port>> {
    let mut matches = Vec::new();
    for port in midi.ports() {
        let name = midi
            .port_name(&port)
            .with_context(|| format!("Could not read MIDI {direction} port name"))?;
        if is_prophet_port(&name) {
            matches.push((name, port));
        }
    }
    unique_port(matches, direction)
}

fn unique_port<T>(mut matches: Vec<(String, T)>, direction: &str) -> Result<Option<T>> {
    ensure!(
        matches.len() <= 1,
        "Multiple Prophet-6 MIDI {direction} ports found: {}. Connect only one Prophet-6.",
        matches
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(matches.pop().map(|(_, port)| port))
}

fn is_prophet_port(name: &str) -> bool {
    let mut words = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .peekable();
    while let Some(word) = words.next() {
        if word.eq_ignore_ascii_case("prophet6")
            || (word.eq_ignore_ascii_case("prophet") && words.peek() == Some(&"6"))
        {
            return true;
        }
    }
    false
}

#[cfg(any(target_os = "macos", test))]
fn physical_ports(
    sources: impl Iterator<Item = (String, u32, Option<u32>)>,
    destinations: impl Iterator<Item = (String, u32, Option<u32>)>,
) -> Result<Option<(u32, u32)>> {
    fn candidates(
        ports: impl Iterator<Item = (String, u32, Option<u32>)>,
    ) -> Vec<(String, (u32, u32))> {
        ports
            .filter_map(|(name, id, device)| {
                (is_prophet_port(&name)).then_some((name, (id, device?)))
            })
            .collect()
    }

    let source = unique_port(candidates(sources), "input")?;
    let destination = unique_port(candidates(destinations), "output")?;
    Ok(match (source, destination) {
        (Some((source, from)), Some((destination, to))) if from == to => {
            Some((source, destination))
        }
        _ => None,
    })
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use coremidi::{Client, Destinations, Object, Sources};
    use coremidi_sys as sys;
    use std::sync::Mutex;

    pub(super) const NO_PORTS: &str =
        "No online Prophet-6 MIDI input/output pair found. Connect it by USB.";

    pub(super) fn prophet_ports() -> Result<Option<(u32, u32)>> {
        // CoreMIDI clients live until process exit; don't create two on every poll.
        static CLIENT: Mutex<Option<Client>> = Mutex::new(None);
        {
            let mut client = CLIENT.lock().unwrap();
            if client.is_none() {
                *client = Some(Client::new(CLIENT_NAME).map_err(|status| {
                    anyhow!("Could not initialize CoreMIDI: OSStatus {status}")
                })?);
            }
        }

        physical_ports(
            Sources
                .into_iter()
                .filter_map(|source| candidate(source.as_ref(), sys::kMIDIObjectType_Source)),
            Destinations.into_iter().filter_map(|destination| {
                candidate(destination.as_ref(), sys::kMIDIObjectType_Destination)
            }),
        )
    }

    pub(super) fn open_output(id: u32) -> Result<MidiOutputConnection> {
        let output = MidiOutput::new(CLIENT_NAME).context("Could not initialize MIDI output")?;
        let port = output.find_port_by_id(id.to_string()).context(NO_PORTS)?;
        connect_output(output, &port)
    }

    fn candidate(
        object: &Object,
        expected_type: sys::MIDIObjectType,
    ) -> Option<(String, u32, Option<u32>)> {
        let name = object.display_name()?;
        let id = object.unique_id()?;
        Some((name, id, online_device(id, expected_type)))
    }

    fn online_device(id: u32, expected_type: sys::MIDIObjectType) -> Option<u32> {
        let mut endpoint = 0;
        let mut kind = 0;
        let mut entity = 0;
        let mut device = 0;
        // These getters write CoreMIDI object refs to local out parameters. An
        // endpoint with no owning entity is virtual, not the direct USB synth.
        unsafe {
            if sys::MIDIObjectFindByUniqueID(id as sys::MIDIUniqueID, &mut endpoint, &mut kind) != 0
                || endpoint == 0
                || kind != expected_type
                || sys::MIDIEndpointGetEntity(endpoint, &mut entity) != 0
                || entity == 0
                || sys::MIDIEntityGetDevice(entity, &mut device) != 0
                || device == 0
            {
                return None;
            }
        }
        [endpoint, entity, device]
            .into_iter()
            .all(is_online)
            .then_some(device)
    }

    fn is_online(object: sys::MIDIObjectRef) -> bool {
        let mut offline = 0;
        // Offline is an optional integer property. A stale object or other
        // read failure is not evidence of an online port.
        match unsafe {
            sys::MIDIObjectGetIntegerProperty(object, sys::kMIDIPropertyOffline, &mut offline)
        } {
            0 => offline == 0,
            sys::kMIDIUnknownProperty => true,
            _ => false,
        }
    }
}

type ProgramDump = (Instant, Vec<u8>);

fn collect_program_dump(pending: &mut Option<ProgramDump>, message: &[u8]) -> Option<ProgramDump> {
    // WinMM can deliver a program dump in multiple callbacks (1024-byte buffers).
    match *message.first()? {
        0xf0 => *pending = Some((Instant::now(), Vec::new())),
        0x00..=0x7f | 0xf7 => {}
        _ => return None,
    }
    let (_, bytes) = pending.as_mut()?;
    bytes.extend_from_slice(message);
    if bytes.ends_with(&[0xf7]) {
        let dump = pending.take()?;
        return sysex::program_dump_slot(&dump.1).map(|_| dump);
    }
    if bytes.len() >= sysex::PROGRAM_DUMP_LEN {
        *pending = None;
    }
    None
}

fn wait_for_program(
    rx: &Receiver<ProgramDump>,
    slot: u16,
    requested_at: Instant,
    timeout: Duration,
) -> Result<Patch> {
    let deadline = requested_at + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match rx.recv_timeout(remaining) {
            Ok((started_at, message)) => {
                if started_at < requested_at || sysex::program_dump_slot(&message) != Some(slot) {
                    continue;
                }
                if started_at >= deadline {
                    break;
                }
                let mut patches = sysex::parse(&message)
                    .with_context(|| format!("Invalid Prophet-6 response for slot {slot:03}"))?;
                ensure!(
                    patches.len() == 1,
                    "Expected one Prophet-6 program dump for slot {slot:03}"
                );
                return Ok(patches.remove(0));
            }
            Err(RecvTimeoutError::Timeout) => break,
            Err(RecvTimeoutError::Disconnected) => {
                bail!("MIDI input closed while waiting for Prophet-6 slot {slot:03}")
            }
        }
    }
    bail!(
        "Timed out waiting for Prophet-6 slot {slot:03}. Check that Global MIDI SysEx is set to USB."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch() -> Patch {
        let mut dump = vec![0xf0, 1, 0x2d, 3];
        dump.extend([0; 1171]);
        dump.push(0xf7);
        sysex::parse(&dump).unwrap().remove(0)
    }

    #[test]
    fn matches_usb_port_names_without_confusing_other_models() {
        for name in [
            "Prophet-6",
            "Prophet-6 Keyboard",
            "MIDIIN2 (Prophet-6)",
            "Prophet-6:Prophet-6 MIDI 1 24:0",
            "PROPHET 6",
            "Prophet6",
        ] {
            assert!(is_prophet_port(name), "{name}");
        }
        for name in [
            "Prophet-600",
            "Prophet-6X",
            "Prophet-5",
            "OB-6",
            "MIDI Interface",
        ] {
            assert!(!is_prophet_port(name), "{name}");
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn rejects_ambiguous_ports_even_when_names_are_identical() {
        struct Ports(Vec<&'static str>);
        impl MidiIO for Ports {
            type Port = &'static str;
            fn ports(&self) -> Vec<Self::Port> {
                self.0.clone()
            }
            fn port_count(&self) -> usize {
                self.0.len()
            }
            fn port_name(&self, port: &Self::Port) -> Result<String, midir::PortInfoError> {
                Ok((*port).to_owned())
            }
        }

        let one = Ports(vec!["Virtual port", "Prophet-6"]);
        assert_eq!(prophet_port(&one, "output").unwrap(), Some("Prophet-6"));
        let both = Ports(vec!["Prophet-6", "Prophet-6"]);
        let error = prophet_port(&both, "output").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Multiple Prophet-6 MIDI output ports")
        );
    }

    #[test]
    fn uses_only_the_online_physical_prophet_pair() {
        let port = |id, device| ("Prophet-6 Keyboard".to_owned(), id, device);
        let available = physical_ports(
            [port(10, None), port(11, Some(100))].into_iter(),
            [port(20, None), port(21, Some(100))].into_iter(),
        );
        assert_eq!(available.unwrap(), Some((11, 21)));

        // Matching names alone do not establish that a physical device is online.
        let gone = physical_ports([port(11, None)].into_iter(), [port(21, None)].into_iter());
        assert_eq!(gone.unwrap(), None);

        let partial = physical_ports(
            [port(11, Some(100))].into_iter(),
            [port(21, None)].into_iter(),
        );
        assert_eq!(partial.unwrap(), None);

        let mismatched = physical_ports(
            [port(11, Some(100))].into_iter(),
            [port(21, Some(200))].into_iter(),
        );
        assert_eq!(mismatched.unwrap(), None);

        let ambiguous = physical_ports(
            [port(11, Some(100))].into_iter(),
            [port(21, Some(100)), port(22, Some(200))].into_iter(),
        );
        assert!(
            ambiguous
                .unwrap_err()
                .to_string()
                .contains("Multiple Prophet-6 MIDI output ports")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn coremidi_packet_buffer_preserves_the_full_audition_frame() {
        let frame = patch().audition();
        // midir constructs this same packet buffer before calling MIDISend.
        let packets = coremidi::PacketBuffer::new(0, &frame);
        let mut packets = packets.iter();
        let packet = packets.next().unwrap();
        assert_eq!(packet.timestamp(), 0);
        assert_eq!(packet.data(), frame);
        assert!(packets.next().is_none());
    }

    #[test]
    fn preflights_the_entire_batch_for_writable_slots() {
        let patch = patch();
        let writes = program_writes(&[(499, patch.clone())]).unwrap();
        assert_eq!(&writes[0][..6], &[0xf0, 1, 0x2d, 2, 4, 99]);
        for slot in [500, 999, 1000] {
            let error = program_writes(&[(0, patch.clone()), (slot, patch.clone())]).unwrap_err();
            assert!(error.to_string().contains("only user slots 000–499"));
        }
    }

    #[test]
    fn reassembles_winmm_fragments_without_including_interleaved_midi() {
        let dump = patch().program(500).unwrap();
        let mut pending = None;
        assert!(collect_program_dump(&mut pending, &dump[..1024]).is_none());
        assert!(collect_program_dump(&mut pending, &[0x90, 64, 100]).is_none());
        let (_, bytes) = collect_program_dump(&mut pending, &dump[1024..]).unwrap();
        assert_eq!(bytes, dump);
        assert!(pending.is_none());
        assert!(collect_program_dump(&mut pending, &dump[1024..]).is_none());
        assert_eq!(collect_program_dump(&mut pending, &dump).unwrap().1, dump);
    }

    #[test]
    fn only_accepts_a_fresh_program_dump_for_the_requested_slot() {
        let (tx, rx) = mpsc::channel();
        let patch = patch();
        let stale_time = Instant::now();
        let requested_at = stale_time + Duration::from_millis(1);
        let target = patch.program(500).unwrap();
        let mut stale = target.clone();
        stale[6] = 1;
        tx.send((stale_time, stale)).unwrap();
        tx.send((requested_at, patch.audition())).unwrap();
        tx.send((requested_at, patch.program(0).unwrap())).unwrap();
        tx.send((requested_at, target.clone())).unwrap();
        let response = wait_for_program(&rx, 500, requested_at, RESPONSE_TIMEOUT).unwrap();
        assert_eq!(response.slot, Some(500));
        assert_eq!(response.program(500).unwrap(), target);
    }

    #[test]
    fn distinguishes_timeout_from_invalid_reply_or_closed_input() {
        let (tx, rx) = mpsc::channel();
        let now = Instant::now();
        tx.send((now, patch().program(6).unwrap())).unwrap();
        let err = wait_for_program(&rx, 7, now, Duration::from_millis(5)).unwrap_err();
        assert!(
            err.to_string()
                .contains("Timed out waiting for Prophet-6 slot 007")
        );

        let now = Instant::now();
        tx.send((now, vec![0xf0, 1, 0x2d, 2, 0, 7, 0xf7])).unwrap();
        let err = wait_for_program(&rx, 7, now, RESPONSE_TIMEOUT).unwrap_err();
        assert!(
            err.to_string()
                .contains("Invalid Prophet-6 response for slot 007")
        );

        drop(tx);
        let err = wait_for_program(&rx, 7, Instant::now(), RESPONSE_TIMEOUT).unwrap_err();
        assert!(err.to_string().contains("MIDI input closed"));
    }
}
