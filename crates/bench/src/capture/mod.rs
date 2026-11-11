//! The project's own capture format (`.ptc`): what the sim published, in compressed ten-second chunks with an
//! index, checksums, and metadata. Writer, replay source (on `pitot_irsdk::capture`), and conversion from
//! `.irdt` and `.ibt`. The format is specified in `docs/capture-format.md`.

pub mod convert;
mod reader;
mod writer;

pub use reader::{CaptureSource, ChunkEntry, Record};
pub use writer::{CaptureWriter, Counts, SourceInfo, WriterOptions};

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use pitot_irsdk::layout::{VarHeader, VarType};

    use super::*;
    use crate::replay::{FrameSource, IrdtSource, Item, ReplayError, SourceLayout};

    const ROW: usize = 16;
    const CLOCK: u64 = 1_000_000;

    fn layout() -> SourceLayout {
        SourceLayout {
            vars: vec![
                VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
                VarHeader::new(VarType::Float, 8, 1, "ThrottleRaw").expect("valid"),
                VarHeader::new(VarType::Int, 12, 1, "Lap").expect("valid"),
            ],
            buf_len: ROW,
            tick_rate: 60,
            session_info_capacity: 4096,
            player_car_only: false,
        }
    }

    fn row(n: u32) -> Vec<u8> {
        let mut row = vec![0u8; ROW];
        row[0..8].copy_from_slice(&(f64::from(n) / 60.0).to_le_bytes());
        row[8..12].copy_from_slice(&(n as f32 / 150.0).to_le_bytes());
        row[12..16].copy_from_slice(&((n / 60) as i32).to_le_bytes());
        row
    }

    fn options() -> WriterOptions {
        WriterOptions {
            level: 3,
            chunk_seconds: 1,
            clock_hz: CLOCK,
            sdk_version: 2,
            source: SourceInfo {
                kind: "test".into(),
                file: None,
                bytes: None,
            },
        }
    }

    /// 150 frames (chunks of 60, 60, and 30), session info at the start and at frame 70 with a gap there, and a
    /// disconnect at frame 140.
    fn written() -> Vec<u8> {
        use std::cell::RefCell;
        use std::io::Write;
        use std::rc::Rc;

        /// `finish` consumes the writer, so the bytes go to a buffer shared with the test.
        #[derive(Clone, Default)]
        struct Shared(Rc<RefCell<Vec<u8>>>);
        impl Write for Shared {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.borrow_mut().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let shared = Shared::default();
        let mut writer = CaptureWriter::new(shared.clone(), &layout(), options()).expect("header");
        let at = |n: u32| u64::from(n) * CLOCK / 60;
        writer
            .session_info(
                0,
                1,
                b"WeekendInfo:
 TrackID: 1
",
            )
            .expect("info");
        for n in 0..150 {
            if n == 70 {
                writer
                    .session_info(
                        at(n),
                        2,
                        b"WeekendInfo:
 TrackID: 2
",
                    )
                    .expect("info");
                writer.gap(at(n), 3).expect("gap");
            }
            if n == 140 {
                writer.disconnect(at(n)).expect("disconnect");
            }
            writer
                .frame(at(n), 500 + n as i32, (n % 3) as i32, &row(n))
                .expect("frame");
        }
        let counts = writer.finish(at(150)).expect("finish");
        assert_eq!(
            (
                counts.frames,
                counts.session_info_revisions,
                counts.gaps,
                counts.missed_ticks
            ),
            (150, 2, 1, 3)
        );
        assert_eq!((counts.disconnects, counts.chunks), (1, 3));
        shared.0.borrow().clone()
    }

    fn items(source: &mut impl FrameSource) -> Vec<(Item, Vec<u8>)> {
        let mut buf = Vec::new();
        let mut out = Vec::new();
        while let Some(item) = source.next(&mut buf).expect("reads") {
            out.push((item, buf.clone()));
        }
        out
    }

    #[test]
    fn writes_and_reads_back_every_record() {
        let bytes = written();
        let mut source = CaptureSource::new(Cursor::new(bytes)).expect("opens");
        assert_eq!(source.layout(), &layout());
        assert_eq!(
            (
                source.clock_hz(),
                source.sdk_version(),
                source.chunk_frames()
            ),
            (CLOCK, 2, 60)
        );

        let records = convert::records(&mut source).expect("records");
        let frames: Vec<_> = records
            .iter()
            .filter_map(|r| match r {
                Record::Frame { time, tick, buffer } => Some((*time, *tick, *buffer)),
                _ => None,
            })
            .collect();
        assert_eq!(frames.len(), 150);
        assert_eq!(frames[0], (0, 500, 0));
        assert_eq!(frames[149], (149 * CLOCK / 60, 649, 2));
        let carried = records
            .iter()
            .filter(|r| matches!(r, Record::SessionInfo { carried: true, .. }))
            .count();
        assert_eq!(
            carried, 2,
            "chunks two and three open with the revision in effect"
        );
        assert!(records.contains(&Record::Gap {
            time: 70 * CLOCK / 60,
            missed: 3
        }));
        assert!(matches!(records.last(), Some(Record::End { time }) if *time == 150 * CLOCK / 60));

        source.rewind().expect("rewinds");
        let replayed = items(&mut source);
        let kinds: Vec<Item> = replayed.iter().map(|(item, _)| *item).collect();
        assert_eq!(kinds.iter().filter(|k| **k == Item::Frame).count(), 150);
        assert_eq!(
            kinds.iter().filter(|k| **k == Item::SessionInfo).count(),
            2,
            "carried copies are not replayed again"
        );
        assert_eq!(kinds.iter().filter(|k| **k == Item::Disconnect).count(), 1);
        let rows: Vec<&Vec<u8>> = replayed
            .iter()
            .filter(|(item, _)| *item == Item::Frame)
            .map(|(_, bytes)| bytes)
            .collect();
        assert!(rows.iter().enumerate().all(|(n, r)| **r == row(n as u32)));
    }

    #[test]
    fn index_seek_and_metadata() {
        let mut source = CaptureSource::new(Cursor::new(written())).expect("opens");
        let index = source.index().expect("reads").expect("has an index");
        assert_eq!(index.len(), 3);
        assert_eq!(
            index
                .iter()
                .map(|c| (c.first_frame, c.frames))
                .collect::<Vec<_>>(),
            [(0, 60), (60, 60), (120, 30)]
        );
        source.seek_chunk(&index[2]);
        let replayed = items(&mut source);
        assert_eq!(
            replayed[0].0,
            Item::SessionInfo,
            "the carried revision comes first"
        );
        assert_eq!(replayed[0].1, b"WeekendInfo:\n TrackID: 2\n");
        assert_eq!(replayed[1], (Item::Frame, row(120)));

        let metadata: serde_json::Value =
            serde_json::from_str(&source.metadata().expect("reads").expect("has metadata"))
                .expect("JSON");
        assert_eq!(metadata["format"], "pitot-capture");
        assert_eq!(metadata["frames"], 150);
        assert_eq!(metadata["missed_ticks"], 3);
        assert_eq!(metadata["seconds"], 2.5);
    }

    #[test]
    fn interrupted_capture_reads_up_to_its_last_whole_chunk() {
        let bytes = written();
        let mut source = CaptureSource::new(Cursor::new(bytes.clone())).expect("opens");
        let index = source.index().expect("reads").expect("index");
        // Cut inside the third chunk: no trailer, no index, and half a chunk.
        let cut = index[2].offset as usize + 60;
        let mut source = CaptureSource::new(Cursor::new(bytes[..cut].to_vec())).expect("opens");
        assert_eq!(source.index().expect("reads"), None);
        let frames = items(&mut source)
            .iter()
            .filter(|(item, _)| *item == Item::Frame)
            .count();
        assert_eq!(frames, 120);
    }

    #[test]
    fn damage_is_detected() {
        let bytes = written();
        let mut source = CaptureSource::new(Cursor::new(bytes.clone())).expect("opens");
        let index = source.index().expect("reads").expect("index");
        let mut header = bytes.clone();
        header[20] ^= 1;
        assert!(matches!(
            CaptureSource::new(Cursor::new(header)),
            Err(ReplayError::Checksum { offset: 0 })
        ));
        let mut payload = bytes.clone();
        let at = index[1].offset as usize + 40 + 20;
        payload[at] ^= 0x55;
        let mut source = CaptureSource::new(Cursor::new(payload)).expect("opens");
        let mut buf = Vec::new();
        let result = loop {
            match source.next(&mut buf) {
                Ok(Some(_)) => continue,
                other => break other,
            }
        };
        assert!(
            matches!(result, Err(ReplayError::Checksum { offset }) if offset == index[1].offset),
            "{result:?}"
        );
        assert!(matches!(
            CaptureSource::new(Cursor::new(
                b"not a capture at all, but long enough to read a header from it.....".to_vec()
            )),
            Err(ReplayError::Format(_))
        ));
    }

    #[test]
    fn irdt_conversion_keeps_every_record() {
        let dir = std::env::temp_dir().join(format!("pitot-capture-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let (input, output) = (dir.join("tape.irdt"), dir.join("tape.ptc"));
        std::fs::write(&input, crate::replay::irdt_tape(90)).expect("write tape");
        let counts = convert::irdt(&input, &output, 3).expect("converts");
        assert_eq!(
            (counts.frames, counts.session_info_revisions, counts.gaps),
            (90, 2, 1)
        );
        assert_eq!(counts.missed_ticks, 2);

        let mut capture = CaptureSource::open(&output).expect("opens");
        let mut irdt = IrdtSource::open(&input).expect("opens tape");
        let checked = convert::check(&mut irdt, &mut capture).expect("identical");
        assert_eq!((checked.frames, checked.session_infos), (90, 2));

        capture.rewind().expect("rewinds");
        let records = convert::records(&mut capture).expect("records");
        let clock = crate::replay::IRDT_TEST_CLOCK_HZ;
        assert_eq!(capture.clock_hz(), clock);
        assert!(records.contains(&Record::Frame {
            time: 45 * clock / 60,
            tick: 1045,
            buffer: 0
        }));
        assert!(records.contains(&Record::Gap {
            time: 45 * clock / 60,
            missed: 2
        }));
        assert!(records.contains(&Record::SessionInfo {
            time: 45 * clock / 60,
            update: 2,
            carried: false
        }));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
