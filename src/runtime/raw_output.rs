use tokio::io::AsyncWriteExt;

use crate::job::JobId;

use super::{Config, RawOutputPolicy};

pub(super) async fn open_raw_log(job_id: JobId, config: &Config) -> Option<tokio::fs::File> {
    match config.raw_output {
        RawOutputPolicy::Persist | RawOutputPolicy::PersistGzipOnFinalize => {}
        RawOutputPolicy::MemoryOnly | RawOutputPolicy::Disabled => return None,
    }

    if tokio::fs::create_dir_all(&config.log_dir).await.is_err() {
        return None;
    }
    tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(config.log_dir.join(format!("{job_id}.log")))
        .await
        .ok()
}

pub(super) async fn finalize_raw_log(
    job_id: JobId,
    config: &Config,
    raw_file: &mut Option<tokio::fs::File>,
) {
    if let Some(file) = raw_file.as_mut() {
        let _ = file.flush().await;
    }
    drop(raw_file.take());

    if !matches!(config.raw_output, RawOutputPolicy::PersistGzipOnFinalize) {
        return;
    }

    let src = config.log_dir.join(format!("{job_id}.log"));
    let dst = config.log_dir.join(format!("{job_id}.log.gz"));
    let _ = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        let mut input = std::fs::File::open(&src)?;
        let output = std::fs::File::create(&dst)?;
        let mut encoder = flate2::write::GzEncoder::new(output, flate2::Compression::default());
        std::io::copy(&mut input, &mut encoder)?;
        encoder.finish()?;
        std::fs::remove_file(src)?;
        Ok(())
    })
    .await;
}
