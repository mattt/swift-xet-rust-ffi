use std::sync::Arc;

use crate::{CasJwtInfo, XetError};

pub struct XetDownloadPlan {
    pub file_info: data::XetFileInfo,
    pub destination: String,
}

impl XetDownloadPlan {
    pub fn new(file_info: data::XetFileInfo, destination: String) -> Self {
        Self {
            file_info,
            destination,
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy)]
pub struct XetDownloadConfig {
    pub chunk_size_bytes: usize,
    pub max_parallel_files: usize,
    pub parallel_failures: usize,
    pub max_retries: usize,
}

impl Default for XetDownloadConfig {
    fn default() -> Self {
        // Read from environment variables if available
        let max_parallel_files = std::env::var("XET_MAX_PARALLEL_FILES")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(32);
        
        let chunk_size_bytes = std::env::var("XET_CHUNK_SIZE_MB")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .map(|mb| mb * 1024 * 1024)
            .unwrap_or(8 * 1024 * 1024);

        Self {
            chunk_size_bytes,
            max_parallel_files,
            parallel_failures: 4,
            max_retries: 3,
        }
    }
}

pub async fn download_with_plan(
    plan: Vec<XetDownloadPlan>,
    jwt: Arc<CasJwtInfo>,
    user_agent: &str,
    config: XetDownloadConfig,
) -> Result<Vec<String>, XetError> {
    let entries: Vec<(data::XetFileInfo, String)> = plan
        .into_iter()
        .map(|entry| (entry.file_info, entry.destination))
        .collect();

    let endpoint = jwt.cas_url();
    let jwt_tuple = (jwt.access_token(), jwt.exp());

    // Configure the data client via environment overrides before first access.
    apply_download_config(config);

    let downloaded = data::data_client::download_async(
        entries,
        Some(endpoint),
        Some(jwt_tuple),
        None,
        None,
        user_agent.to_string(),
    )
    .await?;

    Ok(downloaded)
}

fn apply_download_config(config: XetDownloadConfig) {
    // Set high-performance defaults that work well for typical use cases.
    // Users can override with environment variables if needed.
    
    // Per-file concurrency for range GETs (CRITICAL for single large file throughput)
    // Default to 256 for excellent throughput on modern networks
    if std::env::var("HF_XET_NUM_CONCURRENT_RANGE_GETS").is_err() {
        let default_concurrency = std::env::var("XET_NUM_CONCURRENT_RANGE_GETS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(256);
        std::env::set_var("HF_XET_NUM_CONCURRENT_RANGE_GETS", default_concurrency.to_string());
    }
    
    // Enable high performance mode by default
    if std::env::var("HF_XET_HIGH_PERFORMANCE").is_err() {
        let enable_high_perf = std::env::var("XET_HIGH_PERFORMANCE")
            .ok()
            .as_deref() == Some("0"); // Only disable if explicitly set to "0"
        
        if !enable_high_perf {
            std::env::set_var("HF_XET_HIGH_PERFORMANCE", "1");
        }
    }
    
    // Multi-file downloads
    if std::env::var("HF_XET_MAX_CONCURRENT_DOWNLOADS").is_err() {
        std::env::set_var(
            "HF_XET_MAX_CONCURRENT_DOWNLOADS",
            config.max_parallel_files.to_string(),
        );
    }

    // Ingestion block size (mostly for uploads)
    if std::env::var("HF_XET_INGESTION_BLOCK_SIZE").is_err() {
        std::env::set_var(
            "HF_XET_INGESTION_BLOCK_SIZE",
            config.chunk_size_bytes.to_string(),
        );
    }
}
