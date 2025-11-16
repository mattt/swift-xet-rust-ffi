use hub_client::RepoInfo as HubRepoInfo;
use reqwest::Url;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use uniffi::*;
use urlencoding::encode;

mod xet_download;
mod xet_metadata;

use xet_download::{XetDownloadConfig, XetDownloadPlan};
use xet_metadata::{fetch_file_metadata, get_cached_cas_jwt, FileResolveMetadata, XetFileData};

pub(crate) const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));

/// An error that occurs during Xet operations.
///
/// This error type represents various failure conditions that can occur when
/// interacting with Xet repositories, including network issues, authentication
/// problems, and data processing errors.
#[derive(Debug, thiserror::Error)]
pub enum XetError {
    /// A general operation failure occurred.
    ///
    /// This error indicates that a Xet operation failed for reasons not
    /// covered by the other specific error types.
    #[error("Xet operation failed: {message}")]
    OperationFailed { message: String },

    /// Invalid input was provided to a method.
    ///
    /// This error occurs when method parameters don't meet validation
    /// requirements, such as empty strings or malformed repository identifiers.
    #[error("Invalid input: {message}")]
    InvalidInput { message: String },

    /// An I/O error occurred during file operations.
    ///
    /// This error indicates a problem reading from or writing to the local
    /// file system, such as permission issues or missing directories.
    #[error("IO error: {message}")]
    IoError { message: String },

    /// A network error occurred during a request.
    ///
    /// This error indicates a problem communicating with remote servers,
    /// such as connection failures or HTTP errors.
    #[error("Network error: {message}")]
    NetworkError { message: String },

    /// An authentication error occurred.
    ///
    /// This error indicates that authentication failed, typically due to
    /// invalid or expired credentials.
    #[error("Authentication error: {message}")]
    AuthError { message: String },

    /// A cache operation failed.
    ///
    /// This error indicates a problem accessing or modifying the local
    /// Xet cache directory.
    #[error("Cache error: {message}")]
    CacheError { message: String },

    /// A token-related error occurred.
    ///
    /// This error indicates a problem with authentication tokens, such as
    /// parsing failures or invalid token formats.
    #[error("Token error: {message}")]
    TokenError { message: String },
}

impl From<std::io::Error> for XetError {
    fn from(err: std::io::Error) -> Self {
        XetError::IoError {
            message: err.to_string(),
        }
    }
}

impl From<reqwest::Error> for XetError {
    fn from(err: reqwest::Error) -> Self {
        if err.is_status() {
            let status = err.status().unwrap();
            if status == reqwest::StatusCode::UNAUTHORIZED
                || status == reqwest::StatusCode::FORBIDDEN
            {
                XetError::AuthError {
                    message: format!("Authentication failed: {}", err),
                }
            } else if status.is_client_error() {
                XetError::InvalidInput {
                    message: format!("Client error: {}", err),
                }
            } else {
                XetError::NetworkError {
                    message: format!("HTTP error {}: {}", status, err),
                }
            }
        } else {
            XetError::NetworkError {
                message: format!("Network error: {}", err),
            }
        }
    }
}

impl From<serde_json::Error> for XetError {
    fn from(err: serde_json::Error) -> Self {
        XetError::OperationFailed {
            message: format!("JSON parsing error: {}", err),
        }
    }
}

impl From<hub_client::HubClientError> for XetError {
    fn from(err: hub_client::HubClientError) -> Self {
        XetError::OperationFailed {
            message: format!("Hub client error: {}", err),
        }
    }
}

impl From<data::errors::DataProcessingError> for XetError {
    fn from(err: data::errors::DataProcessingError) -> Self {
        XetError::OperationFailed {
            message: format!("Data processing error: {}", err),
        }
    }
}

impl From<utils::errors::AuthError> for XetError {
    fn from(err: utils::errors::AuthError) -> Self {
        XetError::TokenError {
            message: format!("Authentication error: {}", err),
        }
    }
}

/// A client for interacting with Xet repositories.
///
/// The `XetClient` provides methods to download files, list repository contents,
/// and manage the local cache for Xet-enabled repositories on Hugging Face Hub.
///
/// You can create a client with or without authentication. For public repositories,
/// authentication is optional. For private repositories or upload operations,
/// you need to provide a Hugging Face authentication token.
pub struct XetClient {
    runtime: tokio::runtime::Runtime,
    http_client: reqwest::Client,
    endpoint: String,
    token: Option<String>,
}

// Response types for HF Hub API
#[derive(serde::Deserialize)]
struct TreeEntry {
    path: String,
    #[serde(rename = "type")]
    entry_type: String,
    #[serde(default)]
    oid: Option<String>, // Git object ID, might be used for file access
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    lfs: Option<serde_json::Value>, // LFS pointer info
}

#[derive(serde::Deserialize)]
struct TreeResponse {
    tree: Option<Vec<TreeEntry>>,
}

/// Information about a file stored in a Xet repository.
///
/// This type contains the hash and size of a file, which are used to
/// identify and download files from Xet's content-addressable storage system.
#[derive(Clone)]
pub struct XetFileInfo {
    inner: data::XetFileInfo,
}

impl XetFileInfo {
    /// Creates a new file info instance.
    ///
    /// # Arguments
    ///
    /// * `hash` - The content hash of the file, typically a SHA-256 hash.
    /// * `file_size` - The size of the file in bytes.
    pub fn new(hash: String, file_size: u64) -> Self {
        Self {
            inner: data::XetFileInfo::new(hash, file_size),
        }
    }

    /// Returns the content hash of the file.
    ///
    /// This hash uniquely identifies the file's content and is used to
    /// retrieve the file from Xet's storage system.
    pub fn hash(&self) -> String {
        self.inner.hash().to_string()
    }

    /// Returns the size of the file in bytes.
    pub fn file_size(&self) -> u64 {
        self.inner.file_size()
    }
}

impl From<data::XetFileInfo> for XetFileInfo {
    fn from(inner: data::XetFileInfo) -> Self {
        Self { inner }
    }
}

impl From<XetFileInfo> for data::XetFileInfo {
    fn from(wrapper: XetFileInfo) -> Self {
        wrapper.inner
    }
}

/// Metadata about a file or directory entry in a repository.
///
/// This type provides information about entries in a repository's file tree,
/// including their paths, types, sizes, and content identifiers.
pub struct FileMetadata {
    path: String,
    entry_type: String,
    size: Option<u64>,
    hash: Option<String>,
    oid: Option<String>,
}

impl FileMetadata {
    /// Returns the path of the file or directory within the repository.
    ///
    /// This is a relative path from the repository root.
    pub fn path(&self) -> String {
        self.path.clone()
    }

    /// Returns the type of the entry.
    ///
    /// Common values are `"file"` for files and `"directory"` for directories.
    pub fn entry_type(&self) -> String {
        self.entry_type.clone()
    }

    /// Returns the size of the file in bytes, if available.
    ///
    /// This value is `None` for directories or when size information
    /// is not available.
    pub fn size(&self) -> Option<u64> {
        self.size
    }

    /// Returns the content hash of the file, if available.
    ///
    /// This value is typically present for files stored using Xet or LFS.
    /// It may be `None` for directories or files without hash information.
    pub fn hash(&self) -> Option<String> {
        self.hash.clone()
    }

    /// Returns the Git object ID of the entry, if available.
    ///
    /// This is the SHA-1 hash of the Git object representing this entry.
    pub fn oid(&self) -> Option<String> {
        self.oid.clone()
    }
}

impl From<TreeEntry> for FileMetadata {
    fn from(entry: TreeEntry) -> Self {
        // Try to extract hash from LFS pointer if available
        let hash = entry
            .lfs
            .as_ref()
            .and_then(|lfs| lfs.get("oid"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        Self {
            path: entry.path,
            entry_type: entry.entry_type,
            size: entry.size,
            hash,
            oid: entry.oid,
        }
    }
}

/// JWT token information for accessing the Content-Addressable Storage (CAS) system.
///
/// This type contains the authentication token and endpoint URL needed to
/// interact with Xet's CAS system for downloading or uploading files.
pub struct CasJwtInfo {
    inner: hub_client::CasJWTInfo,
}

impl Clone for CasJwtInfo {
    fn clone(&self) -> Self {
        Self {
            inner: hub_client::CasJWTInfo {
                cas_url: self.inner.cas_url.clone(),
                exp: self.inner.exp,
                access_token: self.inner.access_token.clone(),
            },
        }
    }
}

impl CasJwtInfo {
    /// Returns the URL of the CAS server endpoint.
    ///
    /// Use this URL when making requests to the CAS system.
    pub fn cas_url(&self) -> String {
        self.inner.cas_url.clone()
    }

    /// Returns the JWT access token for authenticating CAS requests.
    ///
    /// Include this token in the `Authorization` header when making
    /// requests to the CAS server.
    pub fn access_token(&self) -> String {
        self.inner.access_token.clone()
    }

    /// Returns the expiration time of the token as a Unix timestamp.
    ///
    /// The token is valid until this time. After expiration, you need to
    /// obtain a new token using `get_cas_jwt`.
    pub fn exp(&self) -> u64 {
        self.inner.exp
    }
}

impl From<hub_client::CasJWTInfo> for CasJwtInfo {
    fn from(inner: hub_client::CasJWTInfo) -> Self {
        Self { inner }
    }
}

/// Progress information for file download or upload operations.
///
/// This type tracks the progress of data transfer operations, including
/// both the total amount of data to transfer and the amount already completed.
pub struct ProgressUpdate {
    total_bytes: u64,
    total_bytes_completed: u64,
    total_transfer_bytes: u64,
    total_transfer_bytes_completed: u64,
}

impl ProgressUpdate {
    /// Returns the total number of bytes to process.
    ///
    /// This represents the total size of all files being transferred.
    pub fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Returns the number of bytes that have been processed.
    ///
    /// This value increases as files are downloaded or uploaded.
    pub fn total_bytes_completed(&self) -> u64 {
        self.total_bytes_completed
    }

    /// Returns the total number of bytes to transfer over the network.
    ///
    /// This may be less than `total_bytes` due to deduplication and compression.
    pub fn total_transfer_bytes(&self) -> u64 {
        self.total_transfer_bytes
    }

    /// Returns the number of bytes that have been transferred over the network.
    ///
    /// This value increases as data is downloaded or uploaded.
    pub fn total_transfer_bytes_completed(&self) -> u64 {
        self.total_transfer_bytes_completed
    }
}

impl From<progress_tracking::ProgressUpdate> for ProgressUpdate {
    fn from(update: progress_tracking::ProgressUpdate) -> Self {
        Self {
            total_bytes: update.total_bytes,
            total_bytes_completed: update.total_bytes_completed,
            total_transfer_bytes: update.total_transfer_bytes,
            total_transfer_bytes_completed: update.total_transfer_bytes_completed,
        }
    }
}

/// A request to download a file from a repository.
///
/// This type encapsulates the parameters needed to download a single file,
/// including the repository identifier, file path, destination, and optional revision.
pub struct FileDownloadRequest {
    repo: String,
    path: String,
    destination: String,
    revision: Option<String>,
}

impl FileDownloadRequest {
    /// Creates a new file download request.
    ///
    /// # Arguments
    ///
    /// * `repo` - The repository identifier (e.g., `"owner/repo"` or `"datasets/owner/repo"`).
    /// * `path` - The path of the file within the repository.
    /// * `destination` - The local file path where the downloaded file should be saved.
    /// * `revision` - An optional Git revision, branch, or tag name. If `None`, defaults to `"main"`.
    pub fn new(repo: String, path: String, destination: String, revision: Option<String>) -> Self {
        Self {
            repo,
            path,
            destination,
            revision,
        }
    }

    /// Returns the repository identifier.
    ///
    /// This can be in the format `"owner/repo"` (defaults to model type) or
    /// `"type/owner/repo"` where type is `models`, `datasets`, or `spaces`.
    pub fn repo(&self) -> String {
        self.repo.clone()
    }

    /// Returns the path of the file within the repository.
    ///
    /// This is a relative path from the repository root.
    pub fn path(&self) -> String {
        self.path.clone()
    }

    /// Returns the local file path where the file will be saved.
    ///
    /// The parent directory will be created if it doesn't exist.
    pub fn destination(&self) -> String {
        self.destination.clone()
    }

    /// Returns the Git revision, branch, or tag name.
    ///
    /// If `None`, the default branch (typically `"main"`) is used.
    pub fn revision(&self) -> Option<String> {
        self.revision.clone()
    }
}

/// Information about a Hugging Face repository.
///
/// This type contains the repository type and full name, which uniquely
/// identify a repository on Hugging Face Hub.
pub struct RepoInfo {
    repo_type: String,
    full_name: String,
}

impl RepoInfo {
    /// Returns the type of the repository.
    ///
    /// Common values are `"model"`, `"dataset"`, and `"space"`.
    pub fn repo_type(&self) -> String {
        self.repo_type.clone()
    }

    /// Returns the full name of the repository.
    ///
    /// This is in the format `"owner/repo"` and uniquely identifies
    /// the repository within its type.
    pub fn full_name(&self) -> String {
        self.full_name.clone()
    }
}

impl From<hub_client::RepoInfo> for RepoInfo {
    fn from(info: hub_client::RepoInfo) -> Self {
        Self {
            repo_type: info.repo_type.as_str().to_string(),
            full_name: info.full_name,
        }
    }
}

/// Statistics about the local Xet cache.
///
/// This type provides information about the cache's size and the number
/// of cached files.
pub struct CacheStats {
    total_size_bytes: u64,
    file_count: u64,
}

impl CacheStats {
    /// Returns the total size of the cache in bytes.
    ///
    /// This includes all cached files and their metadata.
    pub fn total_size_bytes(&self) -> u64 {
        self.total_size_bytes
    }

    /// Returns the number of files in the cache.
    pub fn file_count(&self) -> u64 {
        self.file_count
    }
}

// Progress callback support can be added later if needed
// For now, progress tracking is handled internally by the data crate

/// Checks if pointer file detection should be attempted based on file extension.
///
/// Returns `false` for known binary file extensions like .safetensors, .bin, .pt, etc.
/// to avoid unnecessary UTF-8 parsing and improve performance.
fn should_try_pointer_detection(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    
    !matches!(
        ext.as_str(),
        "safetensors" | "bin" | "pt" | "onnx" | "tflite"
        | "tar" | "gz" | "zip" | "xz" | "zst" | "bz2" 
        | "npy" | "npz" | "h5" | "ckpt" | "pth"
    )
}

impl XetClient {
    /// Creates a new Xet client without authentication.
    ///
    /// Use this initializer to create a client for accessing public repositories.
    /// For private repositories or upload operations, use `with_token` instead.
    ///
    /// # Returns
    ///
    /// A new `XetClient` instance.
    ///
    /// # Errors
    ///
    /// Returns `XetError` if the client cannot be initialized, such as when
    /// the runtime cannot be created.
    pub fn new() -> Result<Self, XetError> {
        // Apply high-performance defaults BEFORE creating the client
        Self::apply_performance_defaults();
        
        let runtime = tokio::runtime::Runtime::new().map_err(|e| XetError::IoError {
            message: format!("Failed to create tokio runtime: {}", e),
        })?;

        let http_client = reqwest::Client::builder()
            .user_agent(concat!(
                env!("CARGO_PKG_NAME"),
                "/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .map_err(|e| XetError::NetworkError {
                message: format!("Failed to create HTTP client: {}", e),
            })?;

        Ok(Self {
            runtime,
            http_client,
            endpoint: "https://huggingface.co".to_string(),
            token: None,
        })
    }

    /// Creates a new Xet client with an authentication token.
    ///
    /// Use this initializer when you need to access private repositories or
    /// perform upload operations. You can obtain a token from your
    /// [Hugging Face account settings](https://huggingface.co/settings/tokens).
    ///
    /// # Arguments
    ///
    /// * `token` - A Hugging Face authentication token. The token must not be empty.
    ///
    /// # Returns
    ///
    /// A new `XetClient` instance configured with the provided token.
    ///
    /// # Errors
    ///
    /// Returns `XetError::InvalidInput` if the token is empty, or `XetError`
    /// if the client cannot be initialized.
    pub fn with_token(token: String) -> Result<Self, XetError> {
        if token.is_empty() {
            return Err(XetError::InvalidInput {
                message: "Token cannot be empty".to_string(),
            });
        }

        // Apply high-performance defaults BEFORE creating the client
        Self::apply_performance_defaults();

        let runtime = tokio::runtime::Runtime::new().map_err(|e| XetError::IoError {
            message: format!("Failed to create tokio runtime: {}", e),
        })?;

        let http_client = reqwest::Client::builder()
            .user_agent(concat!(
                env!("CARGO_PKG_NAME"),
                "/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .map_err(|e| XetError::NetworkError {
                message: format!("Failed to create HTTP client: {}", e),
            })?;

        Ok(Self {
            runtime,
            http_client,
            endpoint: "https://huggingface.co".to_string(),
            token: Some(token),
        })
    }

    /// Get plural form of repo type for API URLs
    fn repo_type_plural(&self, repo_type: &hub_client::HFRepoType) -> &'static str {
        match repo_type {
            hub_client::HFRepoType::Model => "models",
            hub_client::HFRepoType::Dataset => "datasets",
            hub_client::HFRepoType::Space => "spaces",
        }
    }

    /// Parse repository identifier into RepoInfo
    ///
    /// Supports formats:
    /// - "owner/repo" (defaults to model type)
    /// - "models/owner/repo"
    /// - "datasets/owner/repo"
    /// - "spaces/owner/repo"
    fn parse_repo(&self, repo: &str) -> Result<HubRepoInfo, XetError> {
        let parts: Vec<&str> = repo.split('/').collect();

        if parts.len() < 2 {
            return Err(XetError::InvalidInput {
                message: format!("Repository identifier must be in format 'owner/repo' or 'type/owner/repo', got: {}", repo)
            });
        }

        // Check if first part is a repo type
        let (repo_type_str, repo_id) = if parts.len() >= 3 {
            let first = parts[0].to_lowercase();
            if first == "models"
                || first == "model"
                || first == "datasets"
                || first == "dataset"
                || first == "spaces"
                || first == "space"
            {
                (parts[0], parts[1..].join("/"))
            } else {
                ("model", repo.to_string())
            }
        } else {
            ("model", repo.to_string())
        };

        HubRepoInfo::try_from(repo_type_str, &repo_id).map_err(|e| XetError::InvalidInput {
            message: format!("Invalid repository: {}", e),
        })
    }

    /// Returns the version of the Xet client library.
    ///
    /// # Returns
    ///
    /// A string containing the semantic version number (e.g., `"1.0.0"`).
    pub fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    /// Retrieves the content of a file from a Xet repository.
    ///
    /// This method downloads the file content and returns it as raw bytes.
    /// For binary files or large files, consider using `download_file` instead
    /// to save the file directly to disk.
    ///
    /// # Arguments
    ///
    /// * `repo` - The repository identifier (e.g., `"owner/repo"` or `"datasets/owner/repo"`).
    /// * `path` - The path of the file within the repository, relative to the repository root.
    /// * `revision` - An optional Git revision, branch, or tag name. If `None`, defaults to `"main"`.
    ///
    /// # Returns
    ///
    /// The file content as a byte vector.
    ///
    /// # Errors
    ///
    /// Returns `XetError::InvalidInput` if `repo` or `path` is empty, or `XetError::NetworkError`
    /// if the file cannot be retrieved from the repository.
    pub fn get_file_content(
        &self,
        repo: String,
        path: String,
        revision: Option<String>,
    ) -> Result<Vec<u8>, XetError> {
        if repo.is_empty() {
            return Err(XetError::InvalidInput {
                message: "Repository cannot be empty".to_string(),
            });
        }
        if path.is_empty() {
            return Err(XetError::InvalidInput {
                message: "Path cannot be empty".to_string(),
            });
        }

        let repo_info = self.parse_repo(&repo)?;
        let resolved_revision = revision.unwrap_or_else(|| "main".to_string());

        if let Ok(metadata) = self.runtime.block_on(fetch_file_metadata(
            &self.endpoint,
            self.repo_type_plural(&repo_info.repo_type),
            &repo_info.full_name,
            &path,
            &resolved_revision,
            self.token.as_ref(),
        )) {
            if let Ok(bytes) = self.http_get_bytes(&metadata.download_url) {
                return Ok(bytes);
            }
        }

        self.get_file_content_legacy(repo_info, path, resolved_revision)
    }

    /// Lists all files in a directory within a Xet repository.
    ///
    /// This method returns only file paths, not directories. For more detailed
    /// information including file metadata, use `list_files_with_metadata`.
    ///
    /// # Arguments
    ///
    /// * `repo` - The repository identifier (e.g., `"owner/repo"` or `"datasets/owner/repo"`).
    /// * `path` - The directory path within the repository. Use an empty string for the root directory.
    /// * `revision` - An optional Git revision, branch, or tag name. If `None`, defaults to `"main"`.
    ///
    /// # Returns
    ///
    /// An array of file paths, relative to the repository root.
    ///
    /// # Errors
    ///
    /// Returns `XetError::InvalidInput` if `repo` is empty, or `XetError::NetworkError`
    /// if the directory listing cannot be retrieved.
    pub fn list_files(
        &self,
        repo: String,
        path: String,
        revision: Option<String>,
    ) -> Result<Vec<String>, XetError> {
        if repo.is_empty() {
            return Err(XetError::InvalidInput {
                message: "Repository cannot be empty".to_string(),
            });
        }

        let repo_info = self.parse_repo(&repo)?;
        let rev = revision.as_deref().unwrap_or("main");
        let encoded_rev = encode(rev);

        // Build URL for tree API
        let url = if path.is_empty() {
            format!(
                "{}/api/{}/{}/tree/{}",
                self.endpoint,
                self.repo_type_plural(&repo_info.repo_type),
                repo_info.full_name,
                encoded_rev
            )
        } else {
            let encoded_path = encode(&path);
            format!(
                "{}/api/{}/{}/tree/{}/{}",
                self.endpoint,
                self.repo_type_plural(&repo_info.repo_type),
                repo_info.full_name,
                encoded_rev,
                encoded_path
            )
        };

        let file_paths = self.runtime.block_on(async {
            let mut request = self.http_client.get(&url);

            if let Some(token) = &self.token {
                request = request.bearer_auth(token);
            }

            let response = request.send().await.map_err(|e| XetError::from(e))?;
            let response = response.error_for_status().map_err(|e| XetError::from(e))?;
            let body = response.text().await.map_err(|e| XetError::from(e))?;

            // Try to parse as TreeResponse first, then as direct array
            let entries = match serde_json::from_str::<TreeResponse>(&body) {
                Ok(tree_resp) => tree_resp.tree.unwrap_or_default(),
                Err(_) => {
                    // Try parsing as direct array
                    serde_json::from_str::<Vec<TreeEntry>>(&body).map_err(|e| XetError::from(e))?
                }
            };

            Ok::<Vec<String>, XetError>(
                entries
                    .into_iter()
                    .filter(|entry| entry.entry_type == "file")
                    .map(|entry| entry.path)
                    .collect(),
            )
        })?;

        Ok(file_paths)
    }

    /// Lists all entries in a directory within a Xet repository with metadata.
    ///
    /// This method returns both files and directories with their associated metadata,
    /// including sizes, hashes, and Git object IDs when available.
    ///
    /// # Arguments
    ///
    /// * `repo` - The repository identifier (e.g., `"owner/repo"` or `"datasets/owner/repo"`).
    /// * `path` - The directory path within the repository. Use an empty string for the root directory.
    /// * `revision` - An optional Git revision, branch, or tag name. If `None`, defaults to `"main"`.
    ///
    /// # Returns
    ///
    /// An array of `FileMetadata` objects containing information about each entry.
    ///
    /// # Errors
    ///
    /// Returns `XetError::InvalidInput` if `repo` is empty, or `XetError::NetworkError`
    /// if the directory listing cannot be retrieved.
    pub fn list_files_with_metadata(
        &self,
        repo: String,
        path: String,
        revision: Option<String>,
    ) -> Result<Vec<Arc<FileMetadata>>, XetError> {
        if repo.is_empty() {
            return Err(XetError::InvalidInput {
                message: "Repository cannot be empty".to_string(),
            });
        }

        let repo_info = self.parse_repo(&repo)?;
        let rev = revision.as_deref().unwrap_or("main");
        let encoded_rev = encode(rev);

        // Build URL for tree API
        let url = if path.is_empty() {
            format!(
                "{}/api/{}/{}/tree/{}",
                self.endpoint,
                self.repo_type_plural(&repo_info.repo_type),
                repo_info.full_name,
                encoded_rev
            )
        } else {
            let encoded_path = encode(&path);
            format!(
                "{}/api/{}/{}/tree/{}/{}",
                self.endpoint,
                self.repo_type_plural(&repo_info.repo_type),
                repo_info.full_name,
                encoded_rev,
                encoded_path
            )
        };

        let metadata = self.runtime.block_on(async {
            let mut request = self.http_client.get(&url);

            if let Some(token) = &self.token {
                request = request.bearer_auth(token);
            }

            let response = request.send().await.map_err(|e| XetError::from(e))?;
            let response = response.error_for_status().map_err(|e| XetError::from(e))?;
            let body = response.text().await.map_err(|e| XetError::from(e))?;

            // Try to parse as TreeResponse first, then as direct array
            let entries = match serde_json::from_str::<TreeResponse>(&body) {
                Ok(tree_resp) => tree_resp.tree.unwrap_or_default(),
                Err(_) => {
                    // Try parsing as direct array
                    serde_json::from_str::<Vec<TreeEntry>>(&body).map_err(|e| XetError::from(e))?
                }
            };

            Ok::<Vec<Arc<FileMetadata>>, XetError>(
                entries
                    .into_iter()
                    .map(|entry| Arc::new(FileMetadata::from(entry)))
                    .collect(),
            )
        })?;

        Ok(metadata)
    }

    /// Downloads a file from a Xet repository to a local path.
    ///
    /// This method downloads the file content and saves it to the specified destination.
    /// The parent directory of the destination path will be created if it doesn't exist.
    ///
    /// # Arguments
    ///
    /// * `repo` - The repository identifier (e.g., `"owner/repo"` or `"datasets/owner/repo"`).
    /// * `path` - The path of the file within the repository, relative to the repository root.
    /// * `destination` - The local file path where the downloaded file should be saved.
    /// * `revision` - An optional Git revision, branch, or tag name. If `None`, defaults to `"main"`.
    ///
    /// # Errors
    ///
    /// Returns `XetError::InvalidInput` if any parameter is empty, `XetError::IoError`
    /// if the file cannot be written to disk, or `XetError::NetworkError` if the file
    /// cannot be downloaded.
    pub fn download_file(
        &self,
        repo: String,
        path: String,
        destination: String,
        revision: Option<String>,
    ) -> Result<(), XetError> {
        if repo.is_empty() {
            return Err(XetError::InvalidInput {
                message: "Repository cannot be empty".to_string(),
            });
        }
        if path.is_empty() {
            return Err(XetError::InvalidInput {
                message: "Path cannot be empty".to_string(),
            });
        }
        if destination.is_empty() {
            return Err(XetError::InvalidInput {
                message: "Destination cannot be empty".to_string(),
            });
        }

        let repo_info = self.parse_repo(&repo)?;
        let resolved_revision = revision.unwrap_or_else(|| "main".to_string());

        let metadata_result = self.runtime.block_on(fetch_file_metadata(
            &self.endpoint,
            self.repo_type_plural(&repo_info.repo_type),
            &repo_info.full_name,
            &path,
            &resolved_revision,
            self.token.as_ref(),
        ));

        match metadata_result {
            Ok(metadata) => {
                if let Some(xet_data) = metadata.xet_file_data.clone() {
                    if self
                        .runtime
                        .block_on(self.download_with_xet_async(
                            &xet_data,
                            metadata.size,
                            &destination,
                        ))
                        .is_ok()
                    {
                        return Ok(());
                    }
                }

                if self
                    .download_http_with_metadata(&metadata, &destination)
                    .is_ok()
                {
                    return Ok(());
                }

                self.download_file_legacy(repo_info, path, destination, Some(resolved_revision))
            }
            Err(_) => {
                self.download_file_legacy(repo_info, path, destination, Some(resolved_revision))
            }
        }
    }

    /// Downloads multiple files in a single batch operation.
    ///
    /// This method processes download requests sequentially. If any download fails,
    /// the operation stops and returns an error. All successfully downloaded files
    /// are saved before the error is reported.
    ///
    /// # Arguments
    ///
    /// * `requests` - An array of `FileDownloadRequest` objects, each specifying a file to download.
    ///
    /// # Returns
    ///
    /// An array of destination paths for successfully downloaded files.
    ///
    /// # Errors
    ///
    /// Returns `XetError::OperationFailed` if any download fails, with details
    /// about which file failed and why.
    pub fn download_files_batch(
        &self,
        requests: Vec<Arc<FileDownloadRequest>>,
    ) -> Result<Vec<String>, XetError> {
        let mut results = Vec::new();

        for request in requests {
            match self.download_file(
                request.repo(),
                request.path(),
                request.destination(),
                request.revision(),
            ) {
                Ok(_) => results.push(request.destination()),
                Err(e) => {
                    return Err(XetError::OperationFailed {
                        message: format!("Failed to download {}: {}", request.path(), e),
                    });
                }
            }
        }

        Ok(results)
    }

    /// Retrieves a JWT token for accessing the Content-Addressable Storage (CAS) system.
    ///
    /// This method obtains an authentication token that can be used to download or upload
    /// files directly through Xet's CAS system. The token is valid until its expiration time.
    ///
    /// # Arguments
    ///
    /// * `repo` - The repository identifier (e.g., `"owner/repo"` or `"datasets/owner/repo"`).
    /// * `revision` - An optional Git revision, branch, or tag name. If `None`, defaults to `"main"`.
    /// * `is_upload` - `true` for upload operations, `false` for download operations.
    ///
    /// # Returns
    ///
    /// A `CasJwtInfo` object containing the token and CAS server URL.
    ///
    /// # Errors
    ///
    /// Returns `XetError::InvalidInput` if `repo` is empty, `XetError::AuthError` if
    /// authentication fails, or `XetError::NetworkError` if the token cannot be retrieved.
    pub fn get_cas_jwt(
        &self,
        repo: String,
        revision: Option<String>,
        is_upload: bool,
    ) -> Result<Arc<CasJwtInfo>, XetError> {
        if repo.is_empty() {
            return Err(XetError::InvalidInput {
                message: "Repository cannot be empty".to_string(),
            });
        }

        let repo_info = self.parse_repo(&repo)?;
        let operation = if is_upload {
            hub_client::Operation::Upload
        } else {
            hub_client::Operation::Download
        };

        let cred_helper: Arc<dyn hub_client::CredentialHelper> = if let Some(token) = &self.token {
            hub_client::BearerCredentialHelper::new(token.clone(), "swift-xet")
        } else {
            hub_client::NoopCredentialHelper::new()
        };

        let user_agent = self.user_agent();

        let hub_client = hub_client::HubClient::new(
            &self.endpoint,
            repo_info,
            revision,
            user_agent,
            "",
            cred_helper,
        )?;

        let jwt_info = self
            .runtime
            .block_on(async { hub_client.get_cas_jwt(operation).await })?;

        Ok(Arc::new(CasJwtInfo::from(jwt_info)))
    }

    /// Downloads files using the Xet Content-Addressable Storage (CAS) system.
    ///
    /// This method downloads files directly from Xet's CAS system using their content hashes.
    /// Files are written into `destination_dir`, which will be created if missing.
    ///
    /// # Arguments
    ///
    /// * `file_infos` - An array of `XetFileInfo` objects, each containing a file's hash and size.
    /// * `destination_dir` - The local directory where downloaded files should be saved.
    /// * `jwt_info` - A `CasJwtInfo` object describing the CAS endpoint, access token, and expiration.
    ///
    /// # Returns
    ///
    /// An array of file paths for the successfully downloaded files.
    ///
    /// # Errors
    ///
    /// Returns `XetError::InvalidInput` if `file_infos` is empty or `destination_dir` is empty,
    /// `XetError::IoError` if files cannot be written, or `XetError::NetworkError` if downloads fail.
    pub fn download_files(
        &self,
        file_infos: Vec<Arc<XetFileInfo>>,
        destination_dir: String,
        jwt_info: Arc<CasJwtInfo>,
    ) -> Result<Vec<String>, XetError> {
        if file_infos.is_empty() {
            return Err(XetError::InvalidInput {
                message: "File infos cannot be empty".to_string(),
            });
        }
        if destination_dir.is_empty() {
            return Err(XetError::InvalidInput {
                message: "Destination directory cannot be empty".to_string(),
            });
        }

        std::fs::create_dir_all(&destination_dir).map_err(|e| XetError::IoError {
            message: format!("Failed to create destination directory: {}", e),
        })?;

        let plan: Vec<XetDownloadPlan> = file_infos
            .into_iter()
            .enumerate()
            .map(|(i, info)| {
                let data_info = data::XetFileInfo::from((*info).clone());
                let destination = Path::new(&destination_dir)
                    .join(format!("file_{}", i))
                    .to_string_lossy()
                    .to_string();
                XetDownloadPlan::new(data_info, destination)
            })
            .collect();

        let downloaded_paths = self
            .runtime
            .block_on(self.execute_xet_plan(plan, jwt_info.clone()))?;

        Ok(downloaded_paths)
    }

    /// Retrieves file information from a pointer file in the repository.
    ///
    /// This method reads a pointer file (either in Xet JSON format or Git LFS format)
    /// and extracts the file's hash and size information.
    ///
    /// # Arguments
    ///
    /// * `repo` - The repository identifier (e.g., `"owner/repo"` or `"datasets/owner/repo"`).
    /// * `path` - The path to the pointer file within the repository.
    /// * `revision` - An optional Git revision, branch, or tag name. If `None`, defaults to `"main"`.
    ///
    /// # Returns
    ///
    /// A `XetFileInfo` object if the pointer file can be parsed, or `None` if
    /// the file doesn't exist or isn't in a recognized format.
    ///
    /// # Errors
    ///
    /// Returns `XetError::InvalidInput` if `repo` or `path` is empty, or `XetError::NetworkError`
    /// if the pointer file cannot be retrieved.
    pub fn get_file_info(
        &self,
        repo: String,
        path: String,
        revision: Option<String>,
    ) -> Result<Option<Arc<XetFileInfo>>, XetError> {
        let repo_info = self.parse_repo(&repo)?;
        let resolved_revision = revision.unwrap_or_else(|| "main".to_string());

        // First, try to get Xet metadata from HTTP headers (preferred method for HuggingFace)
        // This avoids trying to parse binary files as UTF-8 pointer files
        match self.runtime.block_on(fetch_file_metadata(
            &self.endpoint,
            self.repo_type_plural(&repo_info.repo_type),
            &repo_info.full_name,
            &path,
            &resolved_revision,
            self.token.as_ref(),
        )) {
            Ok(metadata) => {
                eprintln!("✓ Got metadata for {}, size={}, xet_data={}", path, metadata.size, metadata.xet_file_data.is_some());
                // If we have Xet metadata in headers (x-xet-hash), use it directly
                if let Some(xet_data) = metadata.xet_file_data {
                    eprintln!("✓ Using Xet CAS for {} with hash {}", path, xet_data.file_hash);
                    let file_info = data::XetFileInfo::new(xet_data.file_hash, metadata.size);
                    return Ok(Some(Arc::new(XetFileInfo::from(file_info))));
                }
                // Headers present but no Xet data - file is not in Xet CAS
                eprintln!("⚠️  No Xet headers for {}: falling back to pointer file parsing", path);
            }
            Err(e) => {
                eprintln!("Failed to fetch metadata for {}: {}", path, e);
            }
        }

        // Fallback: try to parse as a pointer file (for legacy repos without Xet headers)
        // Skip this for known binary file extensions to avoid UTF-8 errors
        if !should_try_pointer_detection(&path) {
            return Ok(None);
        }

        // Try to get the pointer file content
        let content = self.get_file_content(repo.clone(), path.clone(), Some(resolved_revision.clone()))?;
        
        // Try to convert to UTF-8, but don't fail on binary data
        let content_str = match String::from_utf8(content) {
            Ok(s) => s,
            Err(_) => {
                // Not valid UTF-8, likely a binary file or not a pointer file  
                eprintln!("⚠️  File {} is not valid UTF-8, skipping pointer parsing", path);
                return Ok(None);
            }
        };

        // Try to parse as XetFileInfo JSON
        match serde_json::from_str::<data::XetFileInfo>(&content_str) {
            Ok(file_info) => Ok(Some(Arc::new(XetFileInfo::from(file_info)))),
            Err(_) => {
                // Try to parse as LFS pointer format
                // LFS pointers have format: version <url>\noid <hash>\nsize <size>
                let lines: Vec<&str> = content_str.lines().collect();
                let mut oid = None;
                let mut size = None;

                for line in lines {
                    if line.starts_with("oid ") {
                        oid = Some(line[4..].trim().to_string());
                    } else if line.starts_with("size ") {
                        size = line[5..].trim().parse().ok();
                    }
                }

                if let (Some(hash), Some(file_size)) = (oid, size) {
                    Ok(Some(Arc::new(XetFileInfo::new(hash, file_size))))
                } else {
                    Ok(None)
                }
            }
        }
    }

    /// Parses a repository identifier and returns structured repository information.
    ///
    /// This method validates and parses repository identifiers in various formats,
    /// returning the repository type and full name.
    ///
    /// # Arguments
    ///
    /// * `repo` - The repository identifier, which can be in the format
    ///   `"owner/repo"` (defaults to model type) or `"type/owner/repo"` where type
    ///   is `models`, `datasets`, or `spaces`.
    ///
    /// # Returns
    ///
    /// A `RepoInfo` object containing the repository type and full name.
    ///
    /// # Errors
    ///
    /// Returns `XetError::InvalidInput` if the repository identifier format is invalid.
    pub fn get_repo_info(&self, repo: String) -> Result<Arc<RepoInfo>, XetError> {
        let hub_repo_info = self.parse_repo(&repo)?;
        Ok(Arc::new(RepoInfo::from(hub_repo_info)))
    }

    /// Clears all files from the local Xet cache.
    ///
    /// This method removes all cached files and recreates an empty cache directory.
    /// Use this to free up disk space or to force fresh downloads of cached files.
    ///
    /// # Errors
    ///
    /// Returns `XetError::CacheError` if the cache directory cannot be cleared or recreated.
    pub fn clear_cache(&self) -> Result<(), XetError> {
        let cache_dir = xet_runtime::xet_cache_root();

        // Remove all files in cache directory
        if cache_dir.exists() {
            std::fs::remove_dir_all(&cache_dir).map_err(|e| XetError::CacheError {
                message: format!("Failed to clear cache: {}", e),
            })?;

            // Recreate empty directory
            std::fs::create_dir_all(&cache_dir).map_err(|e| XetError::CacheError {
                message: format!("Failed to recreate cache directory: {}", e),
            })?;
        }

        Ok(())
    }

    /// Returns statistics about the local Xet cache.
    ///
    /// This method calculates the total size and file count of all cached files.
    /// If the cache directory doesn't exist, returns statistics with zero values.
    ///
    /// # Returns
    ///
    /// A `CacheStats` object containing the total cache size in bytes
    /// and the number of cached files.
    ///
    /// # Errors
    ///
    /// Returns `XetError::CacheError` if the cache directory cannot be accessed
    /// or statistics cannot be calculated.
    pub fn get_cache_stats(&self) -> Result<Arc<CacheStats>, XetError> {
        let cache_dir = xet_runtime::xet_cache_root();

        if !cache_dir.exists() {
            return Ok(Arc::new(CacheStats {
                total_size_bytes: 0,
                file_count: 0,
            }));
        }

        let mut total_size: u64 = 0;
        let mut file_count: u64 = 0;

        fn calculate_size(
            path: &Path,
            total_size: &mut u64,
            file_count: &mut u64,
        ) -> std::io::Result<()> {
            if path.is_file() {
                *total_size += path.metadata()?.len();
                *file_count += 1;
            } else if path.is_dir() {
                for entry in std::fs::read_dir(path)? {
                    let entry = entry?;
                    calculate_size(&entry.path(), total_size, file_count)?;
                }
            }
            Ok(())
        }

        calculate_size(&cache_dir, &mut total_size, &mut file_count).map_err(|e| {
            XetError::CacheError {
                message: format!("Failed to calculate cache stats: {}", e),
            }
        })?;

        Ok(Arc::new(CacheStats {
            total_size_bytes: total_size,
            file_count,
        }))
    }

    fn download_file_legacy(
        &self,
        repo_info: HubRepoInfo,
        path: String,
        destination: String,
        revision: Option<String>,
    ) -> Result<(), XetError> {
        let revision = revision.unwrap_or_else(|| "main".to_string());
        let urls_to_try = self.build_resolve_urls(&repo_info, &path, &revision);

        self.runtime.block_on(async {
            let mut last_error = None;

            for url in urls_to_try {
                let mut request = self.http_client.get(&url);

                if let Some(token) = &self.token {
                    request = request.bearer_auth(token);
                }

                match request.send().await {
                    Ok(response) => match response.error_for_status() {
                        Ok(resp) => match resp.bytes().await {
                            Ok(bytes) => {
                                let dest_path = Path::new(&destination);
                                if let Some(parent) = dest_path.parent() {
                                    fs::create_dir_all(parent).map_err(|e| XetError::IoError {
                                        message: format!("Failed to create directory: {}", e),
                                    })?;
                                }

                                fs::write(dest_path, bytes.as_ref()).map_err(|e| {
                                    XetError::IoError {
                                        message: format!("Failed to write file: {}", e),
                                    }
                                })?;

                                return Ok::<(), XetError>(());
                            }
                            Err(e) => {
                                last_error = Some(format!("Failed to read response body: {}", e));
                                continue;
                            }
                        },
                        Err(e) => {
                            last_error = Some(format!("HTTP error: {}", e));
                            continue;
                        }
                    },
                    Err(e) => {
                        last_error = Some(format!("Request error: {}", e));
                        continue;
                    }
                }
            }

            let error_msg = last_error.unwrap_or_else(|| "Unknown error".to_string());
            Err::<(), XetError>(XetError::NetworkError {
                message: format!(
                    "Could not download file. Tried multiple endpoints. Last error: {}",
                    error_msg
                ),
            })
        })?;

        Ok(())
    }

    fn get_file_content_legacy(
        &self,
        repo_info: HubRepoInfo,
        path: String,
        revision: String,
    ) -> Result<Vec<u8>, XetError> {
        let urls_to_try = self.build_resolve_urls(&repo_info, &path, &revision);

        let content = self.runtime.block_on(async {
            let mut last_error = None;

            for url in urls_to_try {
                let mut request = self.http_client.get(&url);

                if let Some(token) = &self.token {
                    request = request.bearer_auth(token);
                }

                match request.send().await {
                    Ok(response) => match response.error_for_status() {
                        Ok(resp) => match resp.bytes().await {
                            Ok(bytes) => return Ok::<Vec<u8>, XetError>(bytes.to_vec()),
                            Err(e) => {
                                last_error = Some(format!("Failed to read response body: {}", e));
                                continue;
                            }
                        },
                        Err(e) => {
                            last_error = Some(format!("HTTP error for {}: {}", url, e));
                            continue;
                        }
                    },
                    Err(e) => {
                        last_error = Some(format!("Request error for {}: {}", url, e));
                        continue;
                    }
                }
            }

            let error_msg = last_error.unwrap_or_else(|| "Unknown error".to_string());
            Err::<Vec<u8>, XetError>(XetError::NetworkError {
                message: format!(
                    "Could not retrieve file. Tried multiple endpoints. Last error: {}",
                    error_msg
                ),
            })
        })?;

        Ok(content)
    }

    fn build_resolve_urls(
        &self,
        repo_info: &HubRepoInfo,
        path: &str,
        revision: &str,
    ) -> Vec<String> {
        let encoded_path = encode(path);
        let encoded_rev = encode(revision);
        let repo_type = &repo_info.repo_type;
        let canonical_prefix = match repo_type {
            hub_client::HFRepoType::Model => "",
            hub_client::HFRepoType::Dataset => "datasets/",
            hub_client::HFRepoType::Space => "spaces/",
        };
        vec![
            format!(
                "{}/{canonical_prefix}{}/resolve/{}/{}",
                self.endpoint, repo_info.full_name, encoded_rev, encoded_path
            ),
            format!(
                "{}/api/{}/{}/resolve/{}/{}",
                self.endpoint,
                self.repo_type_plural(repo_type),
                repo_info.full_name,
                encoded_rev,
                encoded_path
            ),
            format!(
                "{}/api/{}/{}/resolve/{}?revision={}",
                self.endpoint,
                self.repo_type_plural(repo_type),
                repo_info.full_name,
                encoded_path,
                encoded_rev
            ),
        ]
    }

    async fn download_with_xet_async(
        &self,
        xet_data: &XetFileData,
        expected_size: u64,
        destination: &str,
    ) -> Result<(), XetError> {
        self.prepare_destination(destination)?;

        let jwt = get_cached_cas_jwt(
            &self.http_client,
            &xet_data.refresh_route,
            self.token.as_ref(),
        )
        .await?;
        let file_info = data::XetFileInfo::new(xet_data.file_hash.clone(), expected_size);
        let plan = vec![XetDownloadPlan::new(file_info, destination.to_string())];
        self.execute_xet_plan(plan, jwt).await?;
        Ok(())
    }

    fn download_http_with_metadata(
        &self,
        metadata: &FileResolveMetadata,
        destination: &str,
    ) -> Result<(), XetError> {
        let bytes = self.http_get_bytes(&metadata.download_url)?;
        self.write_bytes(destination, &bytes)
    }

    fn http_get_bytes(&self, url: &str) -> Result<Vec<u8>, XetError> {
        let mut request = self.http_client.get(url);
        if self.should_send_auth(url) {
            if let Some(token) = &self.token {
                request = request.bearer_auth(token);
            }
        }

        self.runtime.block_on(async {
            let response = request
                .send()
                .await
                .map_err(|e| XetError::NetworkError {
                    message: format!("Request error: {}", e),
                })?
                .error_for_status()
                .map_err(|e| XetError::NetworkError {
                    message: format!("HTTP error: {}", e),
                })?;

            response
                .bytes()
                .await
                .map(|bytes| bytes.to_vec())
                .map_err(|e| XetError::NetworkError {
                    message: format!("Failed to read response body: {}", e),
                })
        })
    }

    fn write_bytes(&self, destination: &str, bytes: &[u8]) -> Result<(), XetError> {
        self.prepare_destination(destination)?;
        fs::write(destination, bytes).map_err(|e| XetError::IoError {
            message: format!("Failed to write file: {}", e),
        })
    }

    fn prepare_destination(&self, destination: &str) -> Result<(), XetError> {
        let dest_path = Path::new(destination);
        if let Some(parent) = dest_path.parent() {
            fs::create_dir_all(parent).map_err(|e| XetError::IoError {
                message: format!("Failed to create directory: {}", e),
            })?;
        }
        Ok(())
    }

    fn should_send_auth(&self, download_url: &str) -> bool {
        if self.token.is_none() {
            return false;
        }

        match (Url::parse(download_url), Url::parse(&self.endpoint)) {
            (Ok(target), Ok(base)) => target.domain() == base.domain(),
            _ => true,
        }
    }

    fn user_agent(&self) -> &'static str {
        USER_AGENT
    }

    async fn execute_xet_plan(
        &self,
        plan: Vec<XetDownloadPlan>,
        jwt: Arc<CasJwtInfo>,
    ) -> Result<Vec<String>, XetError> {
        xet_download::download_with_plan(plan, jwt, self.user_agent(), XetDownloadConfig::default())
            .await
    }
    
    /// Apply high-performance defaults for downloads.
    /// 
    /// This sets environment variables that the underlying Xet library reads
    /// during initialization to configure optimal download performance.
    fn apply_performance_defaults() {
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
            let disable_high_perf = std::env::var("XET_HIGH_PERFORMANCE")
                .ok()
                .as_deref() == Some("0"); // Only disable if explicitly set to "0"
            
            if !disable_high_perf {
                std::env::set_var("HF_XET_HIGH_PERFORMANCE", "1");
            }
        }
    }
}

// Include the generated UniFFI bindings
uniffi::include_scaffolding!("swift_xet_rust");
