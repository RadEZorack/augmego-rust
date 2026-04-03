use anyhow::{Context, Result, bail};
use chrono::Utc;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::fs;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StorageProvider {
    Local,
    Spaces,
}

#[derive(Clone, Debug)]
pub struct StorageObject {
    pub bytes: Vec<u8>,
    pub content_type: String,
    pub cache_control: Option<String>,
    pub content_encoding: Option<String>,
}

#[derive(Clone, Debug)]
pub struct StorageConfig {
    pub provider: StorageProvider,
    pub root: PathBuf,
    pub namespace: String,
    pub spaces_bucket: String,
    pub spaces_endpoint: String,
    pub spaces_custom_domain: String,
    pub spaces_access_key_id: String,
    pub spaces_secret_access_key: String,
    pub spaces_region: String,
}

#[derive(Clone, Debug)]
pub struct StorageService {
    config: StorageConfig,
    http: Client,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StorageMetadata {
    content_type: String,
    cache_control: Option<String>,
    content_encoding: Option<String>,
}

impl StorageService {
    pub async fn new(config: StorageConfig) -> Result<Self> {
        if config.provider == StorageProvider::Local {
            fs::create_dir_all(&config.root)
                .await
                .with_context(|| format!("create storage root {}", config.root.display()))?;
        }
        Ok(Self {
            config,
            http: Client::new(),
        })
    }

    pub fn namespace(&self) -> &str {
        &self.config.namespace
    }

    pub fn public_url(&self, storage_key: &str) -> Option<String> {
        if self.config.provider != StorageProvider::Spaces {
            return None;
        }

        if !self.config.spaces_custom_domain.trim().is_empty() {
            return Some(format!(
                "{}/{}",
                self.config.spaces_custom_domain.trim_end_matches('/'),
                to_url_safe_storage_key(storage_key)
            ));
        }

        let endpoint = self.config.spaces_endpoint.trim();
        if endpoint.is_empty() || self.config.spaces_bucket.trim().is_empty() {
            return None;
        }

        let endpoint_url = endpoint.trim_end_matches('/');
        let endpoint_host = endpoint_url
            .strip_prefix("https://")
            .or_else(|| endpoint_url.strip_prefix("http://"))?;
        Some(format!(
            "https://{}.{}/{}",
            self.config.spaces_bucket,
            endpoint_host,
            to_url_safe_storage_key(storage_key)
        ))
    }

    pub async fn write_object(
        &self,
        storage_key: &str,
        bytes: &[u8],
        content_type: &str,
        cache_control: Option<&str>,
        content_encoding: Option<&str>,
    ) -> Result<()> {
        match self.config.provider {
            StorageProvider::Local => {
                let path = self.absolute_path(storage_key);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)
                        .await
                        .with_context(|| format!("create storage dir {}", parent.display()))?;
                }
                fs::write(&path, bytes)
                    .await
                    .with_context(|| format!("write storage object {}", path.display()))?;
                let metadata = StorageMetadata {
                    content_type: content_type.to_string(),
                    cache_control: cache_control.map(str::to_string),
                    content_encoding: content_encoding.map(str::to_string),
                };
                let metadata_path = metadata_path(&path);
                let metadata_bytes =
                    serde_json::to_vec(&metadata).context("serialize storage metadata")?;
                fs::write(&metadata_path, metadata_bytes)
                    .await
                    .with_context(|| format!("write storage metadata {}", metadata_path.display()))?;
                Ok(())
            }
            StorageProvider::Spaces => {
                let url = self
                    .spaces_object_url(storage_key)
                    .context("resolve spaces object URL")?;
                let host = self.spaces_host().context("resolve spaces host")?;
                let region = self.spaces_region();
                let access_key = self.config.spaces_access_key_id.trim();
                let secret_key = self.config.spaces_secret_access_key.trim();
                if access_key.is_empty() || secret_key.is_empty() {
                    bail!("DigitalOcean Spaces credentials are not configured");
                }

                let amz_date = Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
                let date_stamp = &amz_date[..8];
                let payload_hash = sha256_hex(bytes);
                let canonical_uri = format!("/{}", to_url_safe_storage_key(storage_key));

                let mut signed_headers = vec![
                    ("content-type", content_type.to_string()),
                    ("host", host.clone()),
                    ("x-amz-acl", "public-read".to_string()),
                    ("x-amz-content-sha256", payload_hash.clone()),
                    ("x-amz-date", amz_date.clone()),
                ];
                if let Some(cache_control) = cache_control {
                    signed_headers.push(("cache-control", cache_control.to_string()));
                }
                if let Some(content_encoding) = content_encoding {
                    signed_headers.push(("content-encoding", content_encoding.to_string()));
                }
                signed_headers.sort_by(|left, right| left.0.cmp(right.0));

                let canonical_headers = signed_headers
                    .iter()
                    .map(|(name, value)| format!("{name}:{}\n", canonicalize_header_value(value)))
                    .collect::<String>();
                let signed_header_names = signed_headers
                    .iter()
                    .map(|(name, _)| *name)
                    .collect::<Vec<_>>()
                    .join(";");

                let canonical_request = format!(
                    "PUT\n{canonical_uri}\n\n{canonical_headers}\n{signed_header_names}\n{payload_hash}"
                );
                let credential_scope = format!("{date_stamp}/{region}/s3/aws4_request");
                let string_to_sign = format!(
                    "AWS4-HMAC-SHA256\n{amz_date}\n{credential_scope}\n{}",
                    sha256_hex(canonical_request.as_bytes())
                );

                let signing_key = aws_v4_signing_key(secret_key, date_stamp, &region, "s3");
                let signature = hex_bytes(&hmac_sha256(&signing_key, string_to_sign.as_bytes()));
                let authorization = format!(
                    "AWS4-HMAC-SHA256 Credential={access_key}/{credential_scope}, SignedHeaders={signed_header_names}, Signature={signature}"
                );

                let mut request = self
                    .http
                    .put(&url)
                    .header("authorization", authorization)
                    .header("content-type", content_type)
                    .header("host", host)
                    .header("x-amz-acl", "public-read")
                    .header("x-amz-content-sha256", payload_hash)
                    .header("x-amz-date", amz_date)
                    .body(bytes.to_vec());
                if let Some(cache_control) = cache_control {
                    request = request.header("cache-control", cache_control);
                }
                if let Some(content_encoding) = content_encoding {
                    request = request.header("content-encoding", content_encoding);
                }

                let response = request.send().await.context("upload object to spaces")?;
                if !response.status().is_success() {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    bail!("Spaces upload failed ({status}): {body}");
                }
                Ok(())
            }
        }
    }

    pub async fn read_object(&self, storage_key: &str) -> Result<Option<StorageObject>> {
        match self.config.provider {
            StorageProvider::Local => {
                let path = self.absolute_path(storage_key);
                let bytes = fs::read(&path).await;
                match bytes {
                    Ok(bytes) => {
                        let metadata = read_metadata(&path).await?;
                        Ok(Some(StorageObject {
                            bytes,
                            content_type: metadata
                                .as_ref()
                                .map(|value| value.content_type.clone())
                                .unwrap_or_else(|| infer_content_type(&path)),
                            cache_control: metadata
                                .as_ref()
                                .and_then(|value| value.cache_control.clone()),
                            content_encoding: metadata
                                .as_ref()
                                .and_then(|value| value.content_encoding.clone()),
                        }))
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                    Err(error) => Err(error)
                        .with_context(|| format!("read storage object {}", path.display())),
                }
            }
            StorageProvider::Spaces => {
                bail!("DigitalOcean Spaces reads are not yet implemented in the Rust runtime")
            }
        }
    }

    pub fn sanitize_filename(value: &str) -> String {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return "model.glb".to_string();
        }

        let sanitized = trimmed
            .chars()
            .map(|ch| match ch {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' | '-' => ch,
                _ => '_',
            })
            .collect::<String>();
        sanitized
            .split('_')
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>()
            .join("_")
            .chars()
            .take(120)
            .collect::<String>()
    }

    fn absolute_path(&self, storage_key: &str) -> PathBuf {
        self.config.root.join(Path::new(storage_key))
    }

    fn spaces_host(&self) -> Option<String> {
        let endpoint = self.config.spaces_endpoint.trim();
        if endpoint.is_empty() || self.config.spaces_bucket.trim().is_empty() {
            return None;
        }
        let endpoint_url = endpoint.trim_end_matches('/');
        let endpoint_host = endpoint_url
            .strip_prefix("https://")
            .or_else(|| endpoint_url.strip_prefix("http://"))
            .unwrap_or(endpoint_url);
        Some(format!("{}.{}", self.config.spaces_bucket.trim(), endpoint_host))
    }

    fn spaces_object_url(&self, storage_key: &str) -> Option<String> {
        let host = self.spaces_host()?;
        Some(format!(
            "https://{host}/{}",
            to_url_safe_storage_key(storage_key)
        ))
    }

    fn spaces_region(&self) -> String {
        if !self.config.spaces_region.trim().is_empty() {
            return self.config.spaces_region.trim().to_string();
        }

        self.config
            .spaces_endpoint
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('.')
            .next()
            .unwrap_or("us-east-1")
            .to_string()
    }
}

pub fn to_url_safe_storage_key(storage_key: &str) -> String {
    storage_key
        .split('/')
        .map(url_encode)
        .collect::<Vec<_>>()
        .join("/")
}

fn url_encode(segment: &str) -> String {
    segment
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~' => vec![byte as char],
            _ => format!("%{byte:02X}").chars().collect::<Vec<_>>(),
        })
        .collect()
}

fn metadata_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.meta.json", path.to_string_lossy()))
}

async fn read_metadata(path: &Path) -> Result<Option<StorageMetadata>> {
    let metadata_path = metadata_path(path);
    match fs::read(&metadata_path).await {
        Ok(bytes) => {
            let metadata = serde_json::from_slice(&bytes)
                .with_context(|| format!("decode storage metadata {}", metadata_path.display()))?;
            Ok(Some(metadata))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error)
            .with_context(|| format!("read storage metadata {}", metadata_path.display())),
    }
}

fn infer_content_type(path: &Path) -> String {
    match path.extension().and_then(|value| value.to_str()) {
        Some("glb") => "model/gltf-binary".to_string(),
        Some("gltf") => "model/gltf+json".to_string(),
        Some("json") => "application/json".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

fn canonicalize_header_value(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn aws_v4_signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let secret = format!("AWS4{secret}");
    let date_key = hmac_sha256(secret.as_bytes(), date.as_bytes());
    let region_key = hmac_sha256(&date_key, region.as_bytes());
    let service_key = hmac_sha256(&region_key, service.as_bytes());
    hmac_sha256(&service_key, b"aws4_request").to_vec()
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK_SIZE: usize = 64;
    let mut key_block = [0u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        let digest = Sha256::digest(key);
        key_block[..digest.len()].copy_from_slice(&digest);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut inner_pad = [0x36u8; BLOCK_SIZE];
    let mut outer_pad = [0x5cu8; BLOCK_SIZE];
    for index in 0..BLOCK_SIZE {
        inner_pad[index] ^= key_block[index];
        outer_pad[index] ^= key_block[index];
    }

    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(data);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_hash);
    outer.finalize().into()
}
