use std::io::Write;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use holtmail_core::Timestamp;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use zip::write::{SimpleFileOptions, StreamWriter};
use zip::{CompressionMethod, DateTime, ZipWriter};

use crate::time::civil;

#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| std::io::Error::other("zip buffer poisoned"))?
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A zip written front to back: entries are compressed on a blocking thread and their bytes are
/// handed to the output as soon as they are made, so only about one entry is held at a time.
pub(crate) struct StreamZip {
    writer: Option<ZipWriter<StreamWriter<Shared>>>,
    buffer: Shared,
}

fn zip_error(error: zip::result::ZipError) -> std::io::Error {
    std::io::Error::other(error.to_string())
}

fn modified(at: Timestamp) -> DateTime {
    let c = civil(at);
    u16::try_from(c.year)
        .ok()
        .and_then(|year| {
            DateTime::from_date_and_time(
                year,
                c.month as u8,
                c.day as u8,
                c.hour as u8,
                c.minute as u8,
                c.second as u8,
            )
            .ok()
        })
        .unwrap_or_default()
}

impl StreamZip {
    pub(crate) fn new() -> Self {
        let buffer = Shared::default();
        Self {
            writer: Some(ZipWriter::new_stream(buffer.clone())),
            buffer,
        }
    }

    pub(crate) async fn add<W: AsyncWrite + Unpin + ?Sized>(
        &mut self,
        out: &mut W,
        name: String,
        at: Timestamp,
        bytes: Bytes,
    ) -> std::io::Result<()> {
        let mut writer = self
            .writer
            .take()
            .ok_or_else(|| std::io::Error::other("the zip already failed"))?;
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .last_modified_time(modified(at))
            .unix_permissions(0o644)
            .large_file(bytes.len() as u64 >= u64::from(u32::MAX));
        let writer = tokio::task::spawn_blocking(move || -> std::io::Result<_> {
            writer.start_file(name, options).map_err(zip_error)?;
            writer.write_all(&bytes)?;
            Ok(writer)
        })
        .await
        .map_err(std::io::Error::other)??;
        self.writer = Some(writer);
        self.drain(out).await
    }

    pub(crate) async fn finish<W: AsyncWrite + Unpin + ?Sized>(
        mut self,
        out: &mut W,
    ) -> std::io::Result<()> {
        let writer = self
            .writer
            .take()
            .ok_or_else(|| std::io::Error::other("the zip already failed"))?;
        tokio::task::spawn_blocking(move || writer.finish().map_err(zip_error))
            .await
            .map_err(std::io::Error::other)??;
        self.drain(out).await
    }

    async fn drain<W: AsyncWrite + Unpin + ?Sized>(&self, out: &mut W) -> std::io::Result<()> {
        let chunk = std::mem::take(
            &mut *self
                .buffer
                .0
                .lock()
                .map_err(|_| std::io::Error::other("zip buffer poisoned"))?,
        );
        out.write_all(&chunk).await
    }
}
