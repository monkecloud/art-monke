use std::time::Duration;

use reqwest::{Body, Response};
use rusty_s3::actions::{DeleteObject, GetObject, PutObject, S3Action};
use rusty_s3::{Bucket, Credentials, UrlStyle};

/// Garage doesn't expose its region per bucket, so it isn't one of the env vars the admin
/// hands out — it just has to match the s3_region set once in the cluster's garage.toml.
const S3_REGION: &str = "garage";

/// Every signed URL here is used by this process immediately and never handed to a client,
/// so it only has to outlive the one request it was signed for.
const SIGNED_URL_TTL: Duration = Duration::from_secs(60);

/// The `.m4a` derivatives the worker writes. Garage stores and returns whatever is set here,
/// and it is what the browser sees on the way back out through `download_audio`.
pub const M4A_CONTENT_TYPE: &str = "audio/mp4";

/// The bucket, the credentials to sign against it, and one HTTP client — bundled because
/// signing without sending is never useful, and both binaries do the identical
/// sign-then-send-then-`error_for_status` dance on every object.
#[derive(Clone)]
pub struct S3Store {
    bucket: Bucket,
    credentials: Credentials,
    http: reqwest::Client,
}

impl S3Store {
    /// Panics on missing or malformed configuration: there is no degraded mode worth
    /// starting in, and both binaries exist to move objects.
    pub fn from_env() -> Self {
        let endpoint = std::env::var("S3_ENDPOINT")
            .expect("set S3_ENDPOINT")
            .parse::<url::Url>()
            .expect("S3_ENDPOINT is not a valid URL");
        let bucket_name = std::env::var("S3_BUCKET").expect("set S3_BUCKET");
        let access_key = std::env::var("S3_ACCESS_KEY").expect("set S3_ACCESS_KEY");
        let secret_key = std::env::var("S3_SECRET_KEY").expect("set S3_SECRET_KEY");

        // Path style, not virtual-host: Garage is reached by IP/NodePort here, not a
        // hostname that a bucket subdomain could be carved out of.
        let bucket = Bucket::new(endpoint, UrlStyle::Path, bucket_name, S3_REGION)
            .expect("S3_ENDPOINT/S3_BUCKET did not form a valid bucket url");

        Self {
            bucket,
            credentials: Credentials::new(access_key, secret_key),
            // A connect timeout and no request timeout. Egress to anything this namespace
            // cannot reach *hangs* rather than erroring, and a worker blocked forever on
            // connect would sit on a claimed job; a transfer, by contrast, is legitimately
            // allowed to take as long as a ~1GB object takes.
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .build()
                .expect("could not build an HTTP client"),
        }
    }

    /// The shared client, for a caller that needs a request this type doesn't cover.
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// GETs an object, optionally forwarding a client `Range` header straight through so
    /// Garage decides what to return (206 + Content-Range, or a plain 200). The response is
    /// handed back unread, so the body can be streamed rather than buffered.
    pub async fn get(&self, key: &str, range: Option<&str>) -> Result<Response, reqwest::Error> {
        let signed_url =
            GetObject::new(&self.bucket, Some(&self.credentials), key).sign(SIGNED_URL_TTL);

        let mut req = self.http.get(signed_url);
        if let Some(range) = range {
            req = req.header(reqwest::header::RANGE, range);
        }
        req.send().await.and_then(Response::error_for_status)
    }

    /// PUTs an object. `body` is a stream as often as not — an incoming request body being
    /// proxied through, or a file being read off disk — so nothing here buffers it.
    pub async fn put(
        &self,
        key: &str,
        content_length: u64,
        content_type: &str,
        body: impl Into<Body>,
    ) -> Result<Response, reqwest::Error> {
        let signed_url =
            PutObject::new(&self.bucket, Some(&self.credentials), key).sign(SIGNED_URL_TTL);

        self.http
            .put(signed_url)
            .header(reqwest::header::CONTENT_LENGTH, content_length)
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(body)
            .send()
            .await
            .and_then(Response::error_for_status)
    }

    /// DELETEs an object. S3 DELETE is idempotent, so this is equally fine for a key that
    /// was never written — which is what makes it usable on a failed upload's cleanup path.
    pub async fn delete(&self, key: &str) -> Result<Response, reqwest::Error> {
        let signed_url =
            DeleteObject::new(&self.bucket, Some(&self.credentials), key).sign(SIGNED_URL_TTL);

        self.http
            .delete(signed_url)
            .send()
            .await
            .and_then(Response::error_for_status)
    }
}
