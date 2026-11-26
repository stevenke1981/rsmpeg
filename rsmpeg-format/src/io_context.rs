use rsmpeg_util::{RsError, RsResult};
use std::fs::File;
use std::io::{BufReader, BufWriter, Cursor, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Capacity of the read/write buffers wrapped around on-disk files.
///
/// Demuxers issue many tiny (1–8 byte) reads while walking box / element /
/// chunk headers; without a buffer every one of those is an OS syscall.
const FILE_BUFFER_CAPACITY: usize = 64 * 1024;

/// I/O abstraction for media file access.
///
/// * [`FileReader`](IOContext::FileReader) — buffered, read-only file input
///   ([`open_file`](IOContext::open_file)).
/// * [`FileWriter`](IOContext::FileWriter) — buffered, write-only file output
///   ([`create_file`](IOContext::create_file)).
/// * [`Buffer`](IOContext::Buffer) — an in-memory cursor that supports both
///   reading and writing ([`from_buffer`](IOContext::from_buffer),
///   [`from_output_buffer`](IOContext::from_output_buffer)).
pub enum IOContext {
    FileReader(BufReader<File>),
    FileWriter(BufWriter<File>),
    Buffer(Cursor<Vec<u8>>),
}

fn unsupported(msg: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Unsupported, msg)
}

impl IOContext {
    /// Open an existing file for buffered reading.
    pub fn open_file(path: impl AsRef<Path>) -> RsResult<Self> {
        let file = File::open(path.as_ref())?;
        Ok(IOContext::FileReader(BufReader::with_capacity(
            FILE_BUFFER_CAPACITY,
            file,
        )))
    }

    /// Create (or truncate) a file for buffered writing.
    pub fn create_file(path: impl AsRef<Path>) -> RsResult<Self> {
        let file = File::create(path.as_ref())?;
        Ok(IOContext::FileWriter(BufWriter::with_capacity(
            FILE_BUFFER_CAPACITY,
            file,
        )))
    }

    /// Wrap an in-memory buffer (readable and writable, positioned at 0).
    pub fn from_buffer(data: Vec<u8>) -> Self {
        IOContext::Buffer(Cursor::new(data))
    }

    /// An empty in-memory output buffer, e.g. for muxing into memory.
    pub fn from_output_buffer() -> Self {
        Self::from_buffer(Vec::new())
    }

    /// Borrow the bytes of an in-memory buffer (`None` for file-backed I/O).
    pub fn buffer_data(&self) -> Option<&[u8]> {
        match self {
            IOContext::Buffer(c) => Some(c.get_ref().as_slice()),
            _ => None,
        }
    }

    /// Consume the context and return its in-memory buffer (`None` for files).
    pub fn into_buffer(self) -> Option<Vec<u8>> {
        match self {
            IOContext::Buffer(c) => Some(c.into_inner()),
            _ => None,
        }
    }

    pub fn read_exact(&mut self, buf: &mut [u8]) -> RsResult<()> {
        match self {
            IOContext::FileReader(r) => r.read_exact(buf)?,
            IOContext::FileWriter(_) => {
                return Err(RsError::Io(unsupported(
                    "cannot read from a write-only file IOContext",
                )))
            }
            IOContext::Buffer(c) => c.read_exact(buf)?,
        }
        Ok(())
    }

    pub fn read_u8(&mut self) -> RsResult<u8> {
        let mut buf = [0u8; 1];
        self.read_exact(&mut buf)?;
        Ok(buf[0])
    }

    pub fn read_u16_be(&mut self) -> RsResult<u16> {
        let mut buf = [0u8; 2];
        self.read_exact(&mut buf)?;
        Ok(u16::from_be_bytes(buf))
    }

    pub fn read_u32_be(&mut self) -> RsResult<u32> {
        let mut buf = [0u8; 4];
        self.read_exact(&mut buf)?;
        Ok(u32::from_be_bytes(buf))
    }

    pub fn read_u64_be(&mut self) -> RsResult<u64> {
        let mut buf = [0u8; 8];
        self.read_exact(&mut buf)?;
        Ok(u64::from_be_bytes(buf))
    }

    pub fn read_u16_le(&mut self) -> RsResult<u16> {
        let mut buf = [0u8; 2];
        self.read_exact(&mut buf)?;
        Ok(u16::from_le_bytes(buf))
    }

    pub fn read_u32_le(&mut self) -> RsResult<u32> {
        let mut buf = [0u8; 4];
        self.read_exact(&mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    /// Seek and return the new absolute position.
    ///
    /// For buffered file input, seeks are expressed relative to the current
    /// position and served with `seek_relative`, so targets that land inside
    /// the current read buffer (the common "skip a few bytes" / "rewind after
    /// peek" patterns) do not discard it.
    pub fn seek(&mut self, pos: SeekFrom) -> RsResult<u64> {
        match self {
            IOContext::FileReader(r) => {
                let delta = match pos {
                    SeekFrom::Current(d) => Some(d),
                    SeekFrom::Start(target) => {
                        let cur = r.stream_position()?;
                        if target >= cur {
                            i64::try_from(target - cur).ok()
                        } else {
                            i64::try_from(cur - target).ok().map(|d| -d)
                        }
                    }
                    SeekFrom::End(_) => None,
                };
                match delta {
                    Some(d) => {
                        r.seek_relative(d)?;
                        Ok(r.stream_position()?)
                    }
                    None => Ok(r.seek(pos)?),
                }
            }
            IOContext::FileWriter(w) => Ok(w.seek(pos)?),
            IOContext::Buffer(c) => Ok(c.seek(pos)?),
        }
    }

    /// Current absolute position. Never discards buffered read data.
    pub fn tell(&mut self) -> RsResult<u64> {
        match self {
            IOContext::FileReader(r) => Ok(r.stream_position()?),
            IOContext::FileWriter(w) => Ok(w.stream_position()?),
            IOContext::Buffer(c) => Ok(c.position()),
        }
    }

    pub fn read_bytes(&mut self, len: usize) -> RsResult<Vec<u8>> {
        let mut buf = vec![0u8; len];
        self.read_exact(&mut buf)?;
        Ok(buf)
    }

    pub fn peek(&mut self, len: usize) -> RsResult<Vec<u8>> {
        let pos = self.tell()?;
        let buf = self.read_bytes(len)?;
        self.seek(SeekFrom::Start(pos))?;
        Ok(buf)
    }

    /// Like [`peek`](Self::peek), but returns *up to* `len` bytes.
    ///
    /// Probing only needs a prefix, so a file shorter than the probe window
    /// (or a stream that ends mid-window) must not be an error — that made
    /// `FormatContext::open_input` reject perfectly valid tiny/truncated
    /// inputs with "failed to fill whole buffer" before the demuxer could
    /// even look at them.
    pub fn peek_prefix(&mut self, len: usize) -> RsResult<Vec<u8>> {
        let pos = self.tell()?;
        let mut buf = vec![0u8; len];
        let mut filled = 0usize;
        while filled < len {
            match self.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    // Restore the position before surfacing I/O failures so a
                    // retry starts from the same place.
                    self.seek(SeekFrom::Start(pos))?;
                    return Err(e.into());
                }
            }
        }
        self.seek(SeekFrom::Start(pos))?;
        buf.truncate(filled);
        Ok(buf)
    }

    /// Read into `buf`, returning the number of bytes read (0 at end of input).
    ///
    /// Unlike [`read_exact`](Self::read_exact) this never fails on a short read.
    pub fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            IOContext::FileReader(r) => r.read(buf),
            IOContext::FileWriter(_) => {
                Err(unsupported("cannot read from a write-only file IOContext"))
            }
            IOContext::Buffer(c) => c.read(buf),
        }
    }

    /// Write the whole buffer at the current position.
    pub fn write_all(&mut self, buf: &[u8]) -> RsResult<()> {
        match self {
            IOContext::FileReader(_) => {
                return Err(RsError::Io(unsupported(
                    "cannot write to a read-only file IOContext",
                )))
            }
            IOContext::FileWriter(w) => w.write_all(buf)?,
            IOContext::Buffer(c) => c.write_all(buf)?,
        }
        Ok(())
    }

    pub fn write_u8(&mut self, val: u8) -> RsResult<()> {
        self.write_all(&[val])
    }

    pub fn write_u16_le(&mut self, val: u16) -> RsResult<()> {
        self.write_all(&val.to_le_bytes())
    }

    pub fn write_u32_le(&mut self, val: u32) -> RsResult<()> {
        self.write_all(&val.to_le_bytes())
    }

    pub fn write_u16_be(&mut self, val: u16) -> RsResult<()> {
        self.write_all(&val.to_be_bytes())
    }

    pub fn write_u32_be(&mut self, val: u32) -> RsResult<()> {
        self.write_all(&val.to_be_bytes())
    }

    pub fn write_u64_be(&mut self, val: u64) -> RsResult<()> {
        self.write_all(&val.to_be_bytes())
    }

    /// Flush buffered output to the underlying file (a no-op for readers and
    /// in-memory buffers).
    pub fn flush(&mut self) -> RsResult<()> {
        match self {
            IOContext::FileReader(_) => {}
            IOContext::FileWriter(w) => w.flush()?,
            IOContext::Buffer(c) => c.flush()?,
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf_ctx() -> IOContext {
        IOContext::from_buffer(vec![0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0])
    }

    #[test]
    fn big_endian_reads_decode_correctly() {
        let mut io = buf_ctx();
        assert_eq!(io.read_u8().unwrap(), 0x12);
        assert_eq!(io.read_u16_be().unwrap(), 0x3456);
        assert_eq!(io.read_u32_be().unwrap(), 0x789a_bcde);
        assert_eq!(io.read_u8().unwrap(), 0xf0);
    }

    #[test]
    fn little_endian_reads_decode_correctly() {
        let mut io = buf_ctx();
        io.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(io.read_u16_le().unwrap(), 0x3412);
        // Next four bytes are 0x56, 0x78, 0x9a, 0xbc → little-endian 0xbc9a7856.
        assert_eq!(io.read_u32_le().unwrap(), 0xbc9a_7856);
    }

    #[test]
    fn read_u64_be_decodes_eight_bytes() {
        let mut io = IOContext::from_buffer(vec![0, 0, 0, 1, 0, 0, 0, 2]);
        assert_eq!(io.read_u64_be().unwrap(), 0x0000_0001_0000_0002);
    }

    #[test]
    fn read_exact_past_end_is_an_error_not_a_panic() {
        let mut io = buf_ctx();
        io.seek(SeekFrom::Start(6)).unwrap();
        assert!(io.read_u32_be().is_err());
        // A partial read must also fail (and not silently return a short value).
        let mut io = buf_ctx();
        io.seek(SeekFrom::Start(7)).unwrap();
        assert!(io.read_u16_be().is_err());
    }

    #[test]
    fn tell_tracks_position_across_reads() {
        let mut io = buf_ctx();
        assert_eq!(io.tell().unwrap(), 0);
        io.read_u16_be().unwrap();
        assert_eq!(io.tell().unwrap(), 2);
        io.seek(SeekFrom::Start(5)).unwrap();
        assert_eq!(io.tell().unwrap(), 5);
    }

    #[test]
    fn peek_does_not_advance_position() {
        let mut io = buf_ctx();
        io.seek(SeekFrom::Start(2)).unwrap();
        let head = io.peek(4).unwrap();
        assert_eq!(head, vec![0x56, 0x78, 0x9a, 0xbc]);
        // Position is unchanged, so the next read sees the same bytes.
        assert_eq!(io.tell().unwrap(), 2);
        assert_eq!(io.read_u32_be().unwrap(), 0x5678_9abc);
    }

    #[test]
    fn read_bytes_zero_length_returns_empty() {
        let mut io = buf_ctx();
        assert!(io.read_bytes(0).unwrap().is_empty());
        assert_eq!(io.tell().unwrap(), 0);
    }

    #[test]
    fn peek_prefix_returns_short_buffer_at_eof_without_error() {
        // A file shorter than the probe window must yield what exists instead
        // of "failed to fill whole buffer".
        let mut io = buf_ctx(); // 8 bytes
        let head = io.peek_prefix(2048).unwrap();
        assert_eq!(head, vec![0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0]);
        // Position is unchanged.
        assert_eq!(io.tell().unwrap(), 0);
    }

    #[test]
    fn peek_prefix_on_empty_input_is_empty() {
        let mut io = IOContext::from_buffer(Vec::new());
        assert!(io.peek_prefix(2048).unwrap().is_empty());
        assert_eq!(io.tell().unwrap(), 0);
    }

    #[test]
    fn peek_prefix_respects_the_start_offset() {
        let mut io = buf_ctx();
        io.seek(SeekFrom::Start(6)).unwrap();
        let head = io.peek_prefix(16).unwrap();
        assert_eq!(head, vec![0xde, 0xf0]);
        assert_eq!(io.tell().unwrap(), 6);
    }

    #[test]
    fn read_returns_zero_at_eof_and_partial_counts() {
        let mut io = buf_ctx();
        let mut buf = [0u8; 4];
        assert_eq!(io.read(&mut buf).unwrap(), 4);
        assert_eq!(&buf, &[0x12, 0x34, 0x56, 0x78]);
        let mut rest = [0u8; 8];
        assert_eq!(io.read(&mut rest).unwrap(), 4, "short read must not error");
        assert_eq!(io.read(&mut rest).unwrap(), 0, "EOF must report 0");
    }

    fn temp_path(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "rsmpeg_ioctx_{}_{nanos}_{name}",
            std::process::id()
        ))
    }

    fn assert_unsupported(err: &RsError) {
        assert!(
            matches!(err, RsError::Io(e) if e.kind() == std::io::ErrorKind::Unsupported),
            "expected Unsupported I/O error, got {err:?}"
        );
    }

    #[test]
    fn buffer_supports_write_then_read_back() {
        let mut io = IOContext::from_output_buffer();
        io.write_u8(0xAB).unwrap();
        io.write_u16_le(0x1234).unwrap();
        io.write_u32_le(0xDEAD_BEEF).unwrap();
        io.write_u16_be(0x5678).unwrap();
        io.write_u32_be(0x0102_0304).unwrap();
        io.write_u64_be(0x1122_3344_5566_7788).unwrap();
        io.flush().unwrap();
        assert_eq!(io.tell().unwrap(), 21);

        io.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(io.read_u8().unwrap(), 0xAB);
        assert_eq!(io.read_u16_le().unwrap(), 0x1234);
        assert_eq!(io.read_u32_le().unwrap(), 0xDEAD_BEEF);
        assert_eq!(io.read_u16_be().unwrap(), 0x5678);
        assert_eq!(io.read_u32_be().unwrap(), 0x0102_0304);
        assert_eq!(io.read_u64_be().unwrap(), 0x1122_3344_5566_7788);
        assert_eq!(io.buffer_data().unwrap().len(), 21);
    }

    #[test]
    fn buffer_overwrite_in_place_patches_bytes() {
        let mut io = IOContext::from_output_buffer();
        io.write_all(&[0u8; 8]).unwrap();
        io.seek(SeekFrom::Start(2)).unwrap();
        io.write_u32_le(0x0403_0201).unwrap();
        io.seek(SeekFrom::End(0)).unwrap();
        io.write_u8(9).unwrap();
        assert_eq!(io.into_buffer().unwrap(), vec![0, 0, 1, 2, 3, 4, 0, 0, 9]);
    }

    #[test]
    fn file_writer_and_reader_round_trip() {
        let path = temp_path("rw.bin");
        {
            let mut w = IOContext::create_file(&path).unwrap();
            assert!(w.buffer_data().is_none());
            w.write_all(b"HEAD").unwrap();
            w.write_u32_le(0).unwrap();
            for i in 0..10_000u32 {
                w.write_u32_be(i).unwrap();
            }
            assert_eq!(w.tell().unwrap(), 8 + 40_000);
            // Patch the placeholder, then return to the end.
            w.seek(SeekFrom::Start(4)).unwrap();
            w.write_u32_le(40_000).unwrap();
            w.seek(SeekFrom::End(0)).unwrap();
            w.flush().unwrap();
            // Reading from a writer is unsupported.
            assert_unsupported(&w.read_u8().unwrap_err());
            let mut b = [0u8; 1];
            assert_eq!(
                w.read(&mut b).unwrap_err().kind(),
                std::io::ErrorKind::Unsupported
            );
            assert!(w.peek_prefix(4).is_err());
        }

        let mut r = IOContext::open_file(&path).unwrap();
        assert_eq!(r.read_bytes(4).unwrap(), b"HEAD");
        assert_eq!(r.read_u32_le().unwrap(), 40_000);
        assert_eq!(r.tell().unwrap(), 8);
        // Sequential reads across the 64 KiB buffer boundary stay exact.
        for i in 0..10_000u32 {
            assert_eq!(r.read_u32_be().unwrap(), i);
        }
        assert_eq!(r.tell().unwrap(), 40_008);
        assert!(r.read_u8().is_err(), "EOF must be an error");

        // Backward / forward / relative / end-relative seeks.
        assert_eq!(r.seek(SeekFrom::Start(8 + 4 * 5000)).unwrap(), 20_008);
        assert_eq!(r.read_u32_be().unwrap(), 5000);
        assert_eq!(r.seek(SeekFrom::Current(-8)).unwrap(), 20_004);
        assert_eq!(r.read_u32_be().unwrap(), 4999);
        assert_eq!(r.seek(SeekFrom::Current(4 * 10)).unwrap(), 20_048);
        assert_eq!(r.read_u32_be().unwrap(), 5010);
        assert_eq!(r.seek(SeekFrom::End(-4)).unwrap(), 40_004);
        assert_eq!(r.read_u32_be().unwrap(), 9999);
        // peek keeps the position.
        r.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(r.peek(4).unwrap(), b"HEAD");
        assert_eq!(r.tell().unwrap(), 0);
        assert_eq!(r.peek_prefix(1 << 20).unwrap().len(), 40_008);
        assert_eq!(r.tell().unwrap(), 0);

        // Writing to a reader is unsupported.
        assert_unsupported(&r.write_u8(1).unwrap_err());
        assert_unsupported(&r.write_all(b"xyz").unwrap_err());
        r.flush().unwrap();
        assert!(r.into_buffer().is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tell_does_not_disturb_buffered_reads() {
        let path = temp_path("tell.bin");
        let data: Vec<u8> = (0..=255u8).cycle().take(200_000).collect();
        std::fs::write(&path, &data).unwrap();
        let mut r = IOContext::open_file(&path).unwrap();
        for (i, &expected) in data.iter().enumerate().take(70_000) {
            assert_eq!(r.tell().unwrap(), i as u64);
            assert_eq!(r.read_u8().unwrap(), expected);
        }
        // Small forward skips inside the buffer stay exact.
        assert_eq!(r.seek(SeekFrom::Current(3)).unwrap(), 70_003);
        assert_eq!(r.read_u8().unwrap(), data[70_003]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_missing_file_is_an_io_error() {
        let err = IOContext::open_file(temp_path("does_not_exist.bin"))
            .err()
            .expect("missing file");
        assert!(matches!(err, RsError::Io(_)), "got {err:?}");
    }
}
