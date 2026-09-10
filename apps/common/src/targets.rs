//! The three AAC tiers, and the handful of names derived from each one.
//!
//! Both binaries have to agree on all of it exactly — the api inserts a `transcode_jobs`
//! row per target and reads the flag column back, the worker parses the bitrate out and
//! writes the derivative object and the progress key. A second copy of any of these
//! mappings would be a silent mismatch rather than a build error.

/// Ascending, and that is the order `list_audio` reports them in.
///
/// Three tiers, topping out at 224k, because that is the most the native `aac` encoder
/// actually delivers — see the encoder note in monke-worker. A nominal 320k tier produced the
/// same ~223k bytes as this one while claiming otherwise, and a 192k tier either side of it
/// was a third transcode and a third object for a difference nobody can hear.
pub const TARGETS: [&str; 3] = ["aac_64", "aac_128", "aac_224"];

/// The `audio_files` column flagging this tier as present in the bucket.
///
/// Returns a `&'static str` from a fixed set rather than formatting the target into a
/// column name: the value reaches SQL as an identifier, where it cannot be a bind
/// parameter, so the only safe version is one that can only ever be a literal from this
/// list.
pub fn target_column(target: &str) -> Option<&'static str> {
    match target {
        "aac_64" => Some("has_aac_64"),
        "aac_128" => Some("has_aac_128"),
        "aac_224" => Some("has_aac_224"),
        _ => None,
    }
}

/// ffmpeg's `-b:a` value: `aac_128` -> `128k`.
pub fn target_bitrate(target: &str) -> Option<String> {
    if !TARGETS.contains(&target) {
        return None;
    }
    let kbps = target.strip_prefix("aac_")?;
    kbps.parse::<u32>().ok().map(|kbps| format!("{kbps}k"))
}

/// Where this tier's object lives, derived from the row's own `s3_key`.
///
/// `s3_key` is `{user_id}/{random_token}` — a hex token with no dot in it — so appending
/// `.{target}.m4a` is unambiguous, and anything holding the column already knows every
/// derivative's key without a second lookup or a parallel naming scheme.
pub fn derivative_key(s3_key: &str, target: &str) -> String {
    format!("{s3_key}.{target}.m4a")
}

/// Redis key *and* pub/sub channel for one in-flight transcode's percentage. The api
/// `PSUBSCRIBE`s `progress:{audio_file_id}:*` to pick up all three at once.
pub fn progress_key(audio_file_id: i64, target: &str) -> String {
    format!("progress:{audio_file_id}:{target}")
}

/// The `{target}` half of a [`progress_key`], for a subscriber that matched the pattern.
///
/// Returns the entry from [`TARGETS`] rather than a slice of the caller's key, so the result
/// outlives the message it was read out of.
pub fn target_from_progress_key(key: &str) -> Option<&'static str> {
    let target = key.rsplit_once(':')?.1;
    TARGETS.iter().copied().find(|known| *known == target)
}
