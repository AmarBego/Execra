use tokio::io::AsyncWriteExt;

use crate::job::JobId;

use super::{RawOutputPolicy, RuntimeConfig};

pub(super) async fn open_raw_log(job_id: JobId, config: &RuntimeConfig) -> Option<tokio::fs::File> {
    let log_dir = config.log_dir.as_ref()?;
    let persist = match config.raw_output {
        RawOutputPolicy::Persist => true,
        #[cfg(feature = "gzip")]
        RawOutputPolicy::PersistGzipOnFinalize => true,
        RawOutputPolicy::MemoryOnly | RawOutputPolicy::Disabled => false,
    };
    if !persist {
        return None;
    }

    if tokio::fs::create_dir_all(log_dir).await.is_err() {
        return None;
    }
    tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join(format!("{job_id}.log")))
        .await
        .ok()
}

pub(super) async fn finalize_raw_log(
    job_id: JobId,
    config: &RuntimeConfig,
    raw_file: &mut Option<tokio::fs::File>,
) {
    if let Some(file) = raw_file.as_mut() {
        let _ = file.flush().await;
    }
    drop(raw_file.take());

    #[cfg(feature = "gzip")]
    {
        if !matches!(config.raw_output, RawOutputPolicy::PersistGzipOnFinalize) {
            return;
        }
        let Some(log_dir) = config.log_dir.as_ref() else {
            return;
        };

        let src = log_dir.join(format!("{job_id}.log"));
        let dst = log_dir.join(format!("{job_id}.log.gz"));
        let _ = tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            let mut input = std::fs::File::open(&src)?;
            let output = std::fs::File::create(&dst)?;
            let mut encoder =
                flate2::write::GzEncoder::new(output, flate2::Compression::default());
            std::io::copy(&mut input, &mut encoder)?;
            encoder.finish()?;
            std::fs::remove_file(src)?;
            Ok(())
        })
        .await;
    }

    #[cfg(not(feature = "gzip"))]
    {
        let _ = (job_id, config);
    }
}
