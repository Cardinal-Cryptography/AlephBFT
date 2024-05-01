use async_trait::async_trait;
use std::marker::Unpin;
use futures::io::{AsyncRead, AsyncWrite};
use futures::{AsyncReadExt, AsyncWriteExt};



#[async_trait]
/// Write backups to peristent storage.
pub trait BackupWriter {
    /// Append new data to the backup.
    async fn append(&mut self, data: &[u8]) -> std::io::Result<()>;
}

#[async_trait]
impl<W: AsyncWrite + Send + Unpin> BackupWriter for W {
    async fn append(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.write_all(data).await?;
        self.flush().await
    }
}


#[async_trait]
pub trait BackupReader {
    /// Read the entire backup.
    async fn read(&mut self) -> std::io::Result<Vec<u8>>;
}

#[async_trait]
impl<R: AsyncRead + Send + Unpin> BackupReader for R {
    async fn read(&mut self) -> std::io::Result<Vec<u8>> {
        let mut buf = Vec::new();
        self.read_to_end(&mut buf).await?;
        Ok(buf)
    }
}
