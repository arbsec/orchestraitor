//! Live-first `models.dev` client implementation.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use reqwest::header::{CONTENT_TYPE, ETAG, HeaderMap, IF_NONE_MATCH};
use tokio::sync::Mutex;
use tokio::time;

use crate::catalog::{CatalogSource, ModelsDevCatalog};
use crate::error::ModelsDevError;

/// Bundled fallback catalog compiled into the binary.
pub const BUNDLED_CATALOG_BYTES: &[u8] = include_bytes!("data/catalog.json");

const MODELS_DEV_CATALOG_URL: &str = "https://models.dev/catalog.json";
const MAX_CATALOG_BYTES: u64 = 10 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const TOTAL_TIMEOUT: Duration = Duration::from_mins(1);
const MEMORY_TTL: Duration = Duration::from_mins(5);
const BACKGROUND_REFRESH_INTERVAL: Duration = Duration::from_hours(1);
const MAX_REDIRECTS: usize = 5;

/// In-house `models.dev` catalog client.
#[derive(Clone)]
pub struct ModelsDevClient {
    http: reqwest::Client,
    endpoint: String,
    cache_dir: Option<PathBuf>,
    state: Arc<Mutex<CatalogState>>,
}

impl ModelsDevClient {
    /// Creates a client using the default `models.dev` catalog endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error when the HTTP client cannot be constructed.
    pub fn new(cache_dir: Option<PathBuf>) -> Result<Self, ModelsDevError> {
        Self::with_endpoint(MODELS_DEV_CATALOG_URL.to_string(), cache_dir)
    }

    /// Creates a client with an explicit endpoint for tests or mirrors.
    ///
    /// # Errors
    ///
    /// Returns an error when the HTTP client cannot be constructed.
    pub fn with_endpoint(
        endpoint: String,
        cache_dir: Option<PathBuf>,
    ) -> Result<Self, ModelsDevError> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TOTAL_TIMEOUT)
            .redirect(reqwest::redirect::Policy::limited(MAX_REDIRECTS))
            .build()
            .map_err(ModelsDevError::HttpClient)?;
        Ok(Self {
            http,
            endpoint,
            cache_dir,
            state: Arc::new(Mutex::new(CatalogState::default())),
        })
    }

    /// Returns a catalog after daemon readiness using live data when possible.
    ///
    /// # Errors
    ///
    /// Returns an error only when live, cached, and bundled catalogs all fail validation.
    pub async fn catalog_after_daemon_ready(&self) -> Result<ModelsDevCatalog, ModelsDevError> {
        if let Some(catalog) = self.fresh_memory_catalog().await {
            return Ok(catalog);
        }
        match self.fetch_live().await {
            Ok(catalog) => Ok(catalog),
            Err(live_error) => self.cached_or_bundled(live_error).await,
        }
    }

    /// Forces an immediate live refresh.
    ///
    /// # Errors
    ///
    /// Returns a download, validation, or cache-write error when refresh fails.
    pub async fn refresh_now(&self) -> Result<ModelsDevCatalog, ModelsDevError> {
        self.fetch_live().await
    }

    /// Starts a background refresh task that never runs on cold startup.
    #[must_use]
    pub fn spawn_background_refresh(&self) -> tokio::task::JoinHandle<()> {
        let client = self.clone();
        tokio::spawn(async move {
            let mut interval = time::interval(BACKGROUND_REFRESH_INTERVAL);
            loop {
                interval.tick().await;
                let _refresh_result = client.refresh_now().await;
            }
        })
    }

    async fn fresh_memory_catalog(&self) -> Option<ModelsDevCatalog> {
        let state = self.state.lock().await;
        let cached = state.catalog.clone()?;
        cached.is_fresh(MEMORY_TTL).then_some(cached)
    }

    async fn fetch_live(&self) -> Result<ModelsDevCatalog, ModelsDevError> {
        let etag = self.state.lock().await.etag.clone();
        let mut request = self.http.get(&self.endpoint);
        if let Some(tag) = etag {
            request = request.header(IF_NONE_MATCH, tag);
        }
        let response = request.send().await.map_err(ModelsDevError::Fetch)?;
        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            return self.mark_not_modified().await;
        }
        let response = response.error_for_status().map_err(ModelsDevError::Fetch)?;
        validate_headers(response.headers())?;
        if let Some(length) = response.content_length()
            && length > MAX_CATALOG_BYTES
        {
            return Err(ModelsDevError::TooLarge(length));
        }
        let headers = response.headers().clone();
        let bytes = response.bytes().await.map_err(ModelsDevError::Fetch)?;
        let length = u64::try_from(bytes.len()).map_err(|_| ModelsDevError::SizeOverflow)?;
        if length > MAX_CATALOG_BYTES {
            return Err(ModelsDevError::TooLarge(length));
        }
        let etag = header_to_string(headers.get(ETAG));
        let catalog = ModelsDevCatalog::from_bytes(&bytes, CatalogSource::Live, etag.clone())?;
        self.store_catalog(catalog.clone(), etag).await?;
        Ok(catalog)
    }

    async fn mark_not_modified(&self) -> Result<ModelsDevCatalog, ModelsDevError> {
        let mut state = self.state.lock().await;
        let catalog = state
            .catalog
            .as_mut()
            .ok_or(ModelsDevError::NotModifiedWithoutCache)?;
        catalog.retrieved_at = SystemTime::now();
        Ok(catalog.clone())
    }

    async fn cached_or_bundled(
        &self,
        live_error: ModelsDevError,
    ) -> Result<ModelsDevCatalog, ModelsDevError> {
        if let Some(catalog) = self.read_latest_cached()? {
            self.store_memory(catalog.clone(), None).await;
            return Ok(catalog);
        }
        let bundled = ModelsDevCatalog::from_bytes(
            BUNDLED_CATALOG_BYTES,
            CatalogSource::BundledFallback,
            None,
        )
        .map_err(|source| ModelsDevError::FallbackFailed {
            live: Box::new(live_error),
            fallback: Box::new(source),
        })?;
        self.store_memory(bundled.clone(), None).await;
        Ok(bundled)
    }

    async fn store_catalog(
        &self,
        catalog: ModelsDevCatalog,
        etag: Option<String>,
    ) -> Result<(), ModelsDevError> {
        if let Some(cache_dir) = &self.cache_dir {
            std::fs::create_dir_all(cache_dir).map_err(ModelsDevError::CacheIo)?;
            let path = cache_dir.join(format!("{}.json", catalog.digest));
            let bytes = serde_json::to_vec(&catalog.raw).map_err(ModelsDevError::Json)?;
            std::fs::write(path, bytes).map_err(ModelsDevError::CacheIo)?;
        }
        self.store_memory(catalog, etag).await;
        Ok(())
    }

    async fn store_memory(&self, catalog: ModelsDevCatalog, etag: Option<String>) {
        let mut state = self.state.lock().await;
        state.catalog = Some(catalog);
        state.etag = etag;
    }

    fn read_latest_cached(&self) -> Result<Option<ModelsDevCatalog>, ModelsDevError> {
        let Some(cache_dir) = &self.cache_dir else {
            return Ok(None);
        };
        let entries = match std::fs::read_dir(cache_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(ModelsDevError::CacheIo(error)),
        };
        let mut newest: Option<(SystemTime, PathBuf)> = None;
        for entry in entries {
            let entry = entry.map_err(ModelsDevError::CacheIo)?;
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .map_err(ModelsDevError::CacheIo)?;
            if newest
                .as_ref()
                .is_none_or(|(current, _path)| modified > *current)
            {
                newest = Some((modified, entry.path()));
            }
        }
        let Some((_modified, path)) = newest else {
            return Ok(None);
        };
        let bytes = std::fs::read(path).map_err(ModelsDevError::CacheIo)?;
        ModelsDevCatalog::from_bytes(&bytes, CatalogSource::Cached, None).map(Some)
    }
}

#[derive(Debug, Default)]
struct CatalogState {
    catalog: Option<ModelsDevCatalog>,
    etag: Option<String>,
}

fn validate_headers(headers: &HeaderMap) -> Result<(), ModelsDevError> {
    let Some(content_type) = headers.get(CONTENT_TYPE) else {
        return Err(ModelsDevError::InvalidContentType);
    };
    let Ok(rendered) = content_type.to_str() else {
        return Err(ModelsDevError::InvalidContentType);
    };
    rendered
        .starts_with("application/json")
        .then_some(())
        .ok_or(ModelsDevError::InvalidContentType)
}

fn header_to_string(value: Option<&reqwest::header::HeaderValue>) -> Option<String> {
    value
        .and_then(|header| header.to_str().ok())
        .map(ToOwned::to_owned)
}
