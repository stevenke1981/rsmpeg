use rsmpeg_util::RsResult;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// I/O abstraction for media file access.
pub enum IOContext {
    File(File),
    Buffer(std::io::Cursor<Vec<u8>>),
}

impl IOContext {
    pub fn open_file(path: impl AsRef<Path>) -> RsResult<Self> {
        Ok(IOContext::File(File::open(path.as_ref())?))
    }

    pub fn from_buffer(data: Vec<u8>) -> Self {
        IOContext::Buffer(std::io::Cursor::new(data))
    }

    pub fn read_exact(&mut self, buf: &mut [u8]) -> RsResult<()> {
        match self {
            IOContext::File(f) => f.read_exact(buf)?,
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

    pub fn seek(&mut self, pos: SeekFrom) -> RsResult<u64> {
        match self {
            IOContext::File(f) => Ok(f.seek(pos)?),
            IOContext::Buffer(c) => Ok(c.seek(pos)?),
        }
    }

    pub fn tell(&mut self) -> RsResult<u64> {
        self.seek(SeekFrom::Current(0))
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
}
