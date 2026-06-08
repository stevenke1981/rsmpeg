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
