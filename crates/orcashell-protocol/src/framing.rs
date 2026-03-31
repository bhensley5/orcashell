use std::io::{self, Read, Write};

const MAX_FRAME_SIZE: usize = 16 * 1024 * 1024; // 16 MiB

pub fn write_frame(writer: &mut dyn Write, bytes: &[u8]) -> io::Result<()> {
    let len: u32 = bytes.len().try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame payload exceeds u32::MAX bytes",
        )
    })?;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(bytes)?;
    writer.flush()
}

pub fn read_frame(reader: &mut dyn Read) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame size {len} exceeds maximum {MAX_FRAME_SIZE}"),
        ));
    }
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf)?;
    Ok(buf)
}
