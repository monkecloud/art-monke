use redis::{Client, IntoConnectionInfo, RedisConnectionInfo};

/// Not an env var, for the same reason S3_REGION isn't: it is fixed by this repo's own
/// k8s/redis.yaml, not handed out by the cluster admin, and the Service name is the same in
/// every namespace.
const REDIS_HOST: &str = "monke-app-redis";
const REDIS_PORT: u16 = 6379;

/// Transcode progress only — never anything that has to survive. k8s/redis.yaml is one pod
/// on one node and is gone for good if that node is lost, so both callers treat every
/// operation against it as best-effort and carry on when it fails.
///
/// Mirrors [`crate::pg_pool`]'s two ways in: one REDIS_URL locally, or REDIS_PASSWORD from
/// the `monke-app-redis` Secret in the cluster. The password is never substituted into a URL
/// string — it is set on the parsed connection info instead, so a `@` or `/` or `#` in it
/// cannot silently re-point the host the way it would in `redis://default:<pw>@host`.
///
/// The username is `default`, not empty: a `requirepass`-only server answers the
/// `redis://:<password>@...` form with `WRONGPASS`, which reads like a wrong password.
pub fn redis_client() -> Client {
    if let Ok(url) = std::env::var("REDIS_URL") {
        return Client::open(url).expect("REDIS_URL is not a valid Redis connection string");
    }

    let password = std::env::var("REDIS_PASSWORD").expect("set REDIS_URL, or REDIS_PASSWORD");

    // Parsed from a credential-free URL purely to get a ConnectionInfo to build on; the
    // credentials are attached afterwards.
    let info = format!("redis://{REDIS_HOST}:{REDIS_PORT}/0")
        .into_connection_info()
        .expect("built-in Redis host/port did not form a valid connection string")
        .set_redis_settings(
            RedisConnectionInfo::default()
                .set_username("default")
                .set_password(password)
                .set_db(0),
        );

    // Deliberately does not include the error's own text: it can quote the connection
    // info, and this one carries the password.
    Client::open(info).expect("could not build a Redis client from REDIS_PASSWORD")
}
