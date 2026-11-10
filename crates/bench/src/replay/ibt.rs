//! `.ibt` files as a replay source: the session info snapshot first, then every record.
//!
//! `.ibt` files hold the player's car only, so per-car widgets have no data in this replay.

use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::Path;

use pitot_irsdk::ibt::{IbtReader, Record};

use super::{FrameSource, Item, ReplayError, SIM_SESSION_INFO_CAPACITY, SourceLayout};

/// An `.ibt` file read sequentially.
#[derive(Debug)]
pub struct IbtSource<R> {
    reader: IbtReader<R>,
    record: Record,
    layout: SourceLayout,
    sent_session_info: bool,
}

impl IbtSource<BufReader<File>> {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ReplayError> {
        Self::new(IbtReader::open(path)?)
    }
}

impl<R: Read + Seek> IbtSource<R> {
    pub fn new(reader: IbtReader<R>) -> Result<Self, ReplayError> {
        let layout = SourceLayout {
            vars: reader.vars().to_vec(),
            buf_len: reader.record_len(),
            tick_rate: reader.header().tick_rate,
            // The file stores only the text; the live sim reserves a larger area.
            session_info_capacity: SIM_SESSION_INFO_CAPACITY.max(reader.session_info().len() + 1),
            player_car_only: true,
        };
        Ok(Self {
            record: reader.new_record(),
            reader,
            layout,
            sent_session_info: false,
        })
    }
}

impl<R: Read + Seek> IbtSource<R> {
    pub fn sdk_version(&self) -> i32 {
        self.reader.header().ver
    }
}

impl<R: Read + Seek> FrameSource for IbtSource<R> {
    fn layout(&self) -> &SourceLayout {
        &self.layout
    }

    fn next(&mut self, buf: &mut Vec<u8>) -> Result<Option<Item>, ReplayError> {
        buf.clear();
        if !self.sent_session_info {
            self.sent_session_info = true;
            buf.extend_from_slice(self.reader.session_info());
            return Ok(Some(Item::SessionInfo));
        }
        if !self.reader.next_record(&mut self.record)? {
            return Ok(None);
        }
        buf.extend_from_slice(self.record.bytes());
        Ok(Some(Item::Frame))
    }

    fn rewind(&mut self) -> Result<(), ReplayError> {
        self.reader.rewind();
        self.sent_session_info = false;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use pitot_irsdk::layout::{DiskSubHeader, Header, SDK_VERSION, VarHeader, VarType};
    use std::io::Cursor;

    const YAML: &[u8] = b"WeekendInfo:\n TrackName: synthetic\n";

    fn ibt() -> Vec<u8> {
        ibt_with(YAML)
    }

    /// A minimal `.ibt`: two variables, the session info text `yaml`, and three records.
    pub(crate) fn ibt_with(yaml: &[u8]) -> Vec<u8> {
        let vars = [
            VarHeader::new(VarType::Double, 0, 1, "SessionTime").expect("valid"),
            VarHeader::new(VarType::Int, 8, 1, "SessionNum").expect("valid"),
        ];
        let table_at = Header::SIZE + DiskSubHeader::SIZE;
        let info_at = table_at + vars.len() * VarHeader::SIZE;
        let data_at = info_at + yaml.len();
        let mut bytes = vec![0u8; data_at + 3 * 12];
        let mut header = Header::default();
        header.ver = SDK_VERSION;
        header.tick_rate = 60;
        header.session_info_len = yaml.len() as i32;
        header.session_info_offset = info_at as i32;
        header.num_vars = 2;
        header.var_header_offset = table_at as i32;
        header.num_buf = 1;
        header.buf_len = 12;
        header.var_buf[0].buf_offset = data_at as i32;
        header.write(&mut bytes).expect("fits");
        let disk = DiskSubHeader {
            session_record_count: 3,
            ..DiskSubHeader::default()
        };
        disk.write(&mut bytes).expect("fits");
        for (n, var) in vars.iter().enumerate() {
            var.write(&mut bytes[table_at + n * VarHeader::SIZE..])
                .expect("fits");
        }
        bytes[info_at..data_at].copy_from_slice(yaml);
        for n in 0..3 {
            let at = data_at + n * 12;
            bytes[at..at + 8].copy_from_slice(&(n as f64).to_le_bytes());
        }
        bytes
    }

    #[test]
    fn yields_session_info_then_records() {
        let reader = IbtReader::new(Cursor::new(ibt())).expect("opens");
        let mut source = IbtSource::new(reader).expect("source");
        let layout = source.layout().clone();
        assert!(layout.player_car_only);
        assert_eq!((layout.buf_len, layout.tick_rate), (12, 60));
        assert_eq!(layout.session_info_capacity, SIM_SESSION_INFO_CAPACITY);
        let mut buf = Vec::new();
        assert_eq!(
            source.next(&mut buf).expect("reads"),
            Some(Item::SessionInfo)
        );
        assert_eq!(buf, YAML);
        for n in 0..3 {
            assert_eq!(source.next(&mut buf).expect("reads"), Some(Item::Frame));
            assert_eq!(buf[0..8], (n as f64).to_le_bytes());
        }
        assert_eq!(source.next(&mut buf).expect("reads"), None);
        source.rewind().expect("rewinds");
        assert_eq!(
            source.next(&mut buf).expect("reads"),
            Some(Item::SessionInfo)
        );
        assert_eq!(source.next(&mut buf).expect("reads"), Some(Item::Frame));
        assert_eq!(buf[0..8], 0.0f64.to_le_bytes());
    }
}
