use anyhow::{Context, Result};
use shared_protocol::{ProtocolError, decode, frame};
use std::io::ErrorKind;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub async fn read_message<T: for<'de> serde::Deserialize<'de>>(stream: &mut TcpStream) -> Result<T> {
    let mut length_bytes = [0_u8; 4];
    stream.read_exact(&mut length_bytes).await.context("read frame length")?;
    let length = u32::from_le_bytes(length_bytes) as usize;
    let mut buffer = vec![0_u8; length];
    stream.read_exact(&mut buffer).await.context("read frame payload")?;
    decode(&buffer).map_err(anyhow::Error::from)
}

pub async fn write_message<T: serde::Serialize>(stream: &mut TcpStream, message: &T) -> Result<()> {
    let bytes = frame(message).map_err(anyhow::Error::from)?;
    stream.write_all(&bytes).await.context("write frame")?;
    Ok(())
}

pub fn is_disconnect(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .map(|io| matches!(io.kind(), ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset | ErrorKind::BrokenPipe))
        .unwrap_or_else(|| error.downcast_ref::<ProtocolError>().is_some())
}
