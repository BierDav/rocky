use super::{Failure, SessionStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const MAX_MESSAGE: usize = 12 * 1024 * 1024;

pub async fn write(
    stream: &mut (impl SessionStream + ?Sized),
    bytes: &[u8],
) -> Result<(), Failure> {
    if bytes.is_empty() || bytes.len() > MAX_MESSAGE {
        return Err("message quota".into());
    }
    stream.write_u32(u32::try_from(bytes.len())?).await?;
    stream.write_all(bytes).await?;
    stream.flush().await?;
    Ok(())
}

pub async fn read(stream: &mut (impl SessionStream + ?Sized)) -> Result<Vec<u8>, Failure> {
    read_bounded(stream, MAX_MESSAGE).await
}

pub async fn read_bounded(
    stream: &mut (impl SessionStream + ?Sized),
    limit: usize,
) -> Result<Vec<u8>, Failure> {
    let length = stream.read_u32().await? as usize;
    if length == 0 || length > limit {
        return Err("message quota".into());
    }
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).await?;
    Ok(bytes)
}

pub async fn serve(
    mut stream: super::Stream,
    incoming: &tokio::sync::mpsc::Sender<super::Incoming>,
) -> Result<(), Failure> {
    let bytes = read(&mut stream).await?;
    let (request, answer, delivered) = super::Request::channel(bytes);
    incoming
        .try_send(super::Incoming::Request(request))
        .map_err(|_| "RPC admission queue is closed or full")?;
    write(&mut stream, &answer.await?).await?;
    stream.shutdown().await?;
    let mut rest = [0; 1];
    let _ = stream.read(&mut rest).await;
    let _ = delivered.send(());
    Ok(())
}
