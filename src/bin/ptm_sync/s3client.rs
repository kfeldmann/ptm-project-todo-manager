//! Thin S3 wrapper for the three operations ptm-sync needs:
//! HEAD (ETag check), GET (download), PUT (upload).
//!
//! Uses rust-s3 in sync/blocking mode (no tokio required).

use anyhow::{Context, Result};
use s3::creds::Credentials;
use s3::serde_types::HeadObjectResult;
use s3::{Bucket, Region};

/// The fixed S3 object name within the prefix.
const OBJECT_NAME: &str = "ptm-backup.zip.enc";

pub struct S3Client {
    bucket: Box<Bucket>,
    s3_key: String,
}

impl S3Client {
    /// Build a client from environment-provided credentials and region.
    ///
    /// Region is read from `$AWS_REGION` (or `$AWS_DEFAULT_REGION`), falling
    /// back to `"us-east-1"`.  Credentials follow the standard AWS credential
    /// chain: environment variables → `~/.aws/credentials` (using `$AWS_PROFILE`).
    pub fn new(bucket_name: &str, prefix: &str) -> Result<Self> {
        let region_str = std::env::var("AWS_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|_| "us-east-1".to_string());

        let region: Region = region_str
            .parse()
            .with_context(|| format!("Invalid AWS region: '{}'", region_str))?;

        // `Credentials::default()` always reads the [default] profile and
        // ignores $AWS_PROFILE.  Read the variable ourselves and pass the profile
        // name explicitly so the standard credential chain still runs first
        // (env vars AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY take priority).
        let profile = std::env::var("AWS_PROFILE").ok();
        let credentials =
            Credentials::new(None, None, None, None, profile.as_deref())
                .context("Failed to load AWS credentials")?;

        let bucket = Bucket::new(bucket_name, region, credentials)
            .with_context(|| format!("Failed to initialise S3 bucket '{}'", bucket_name))?;

        let s3_key = format!("{}/{}", prefix.trim_end_matches('/'), OBJECT_NAME);

        Ok(Self { bucket, s3_key })
    }

    // ── HEAD ─────────────────────────────────────────────────────────────────

    /// Return the ETag of the backup object, or `None` if it does not exist.
    /// Propagates errors for network failures or unexpected HTTP status codes.
    pub fn head_etag(&self) -> Result<Option<String>> {
        let (result, status): (HeadObjectResult, u16) = self
            .bucket
            .head_object(&self.s3_key)
            .context("S3 HEAD request failed")?;

        match status {
            200 => {
                // ETag comes from the HTTP response header; S3 includes surrounding quotes.
                let etag = result.e_tag.map(|s| s.trim_matches('"').to_string());
                Ok(etag)
            }
            404 => Ok(None),
            // 403 on HeadObject means the object doesn't exist AND the caller
            // lacks s3:ListBucket on the bucket (S3 hides non-existence in that
            // case).  Treat as "not found"; the subsequent GET/PUT will surface
            // a real access-denied error if permissions are truly wrong.
            // Fix: add s3:ListBucket on the bucket ARN (not the object ARN).
            403 => Ok(None),
            code => anyhow::bail!("Unexpected HTTP status {} from S3 HEAD", code),
        }
    }

    // ── GET ──────────────────────────────────────────────────────────────────

    /// Download the backup object and return its raw bytes.
    pub fn download(&self) -> Result<Vec<u8>> {
        let response = self
            .bucket
            .get_object(&self.s3_key)
            .context("S3 GET request failed")?;

        if response.status_code() != 200 {
            anyhow::bail!(
                "Unexpected HTTP status {} from S3 GET",
                response.status_code()
            );
        }

        Ok(response.to_vec())
    }

    // ── PUT ──────────────────────────────────────────────────────────────────

    /// Upload `data` to S3 and return the ETag assigned by S3.
    /// Uses the PUT response ETag first; falls back to a HEAD if the PUT
    /// response does not include one.
    pub fn upload(&self, data: &[u8]) -> Result<String> {
        let response = self
            .bucket
            .put_object(&self.s3_key, data)
            .context("S3 PUT request failed")?;

        if response.status_code() != 200 {
            anyhow::bail!(
                "Unexpected HTTP status {} from S3 PUT",
                response.status_code()
            );
        }

        // Try to read the ETag directly from the PUT response headers.
        let headers = response.headers();
        if let Some(etag) = headers.get("etag").map(|s| s.trim_matches('"').to_string()) {
            if !etag.is_empty() {
                return Ok(etag);
            }
        }

        // Fall back to a HEAD request (the plan says "store what comes from S3").
        self.head_etag()
            .context("Failed to fetch ETag after upload")?
            .context("Object not found immediately after upload")
    }
}
