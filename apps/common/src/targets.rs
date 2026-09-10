//! The AAC tier, and the handful of names derived from it.
//!
//! Both binaries have to agree on all of it exactly — the api inserts a `transcode_jobs`
//! row per target and reads the flag column back, the worker parses the bitrate out and
//! writes the derivative object and the progress key. A second copy of any of these
//! mappings would be a silent mismatch rather than a build error.

/// Ascending, and that is the order `list_audio` reports them in.
///
/// 224k because that is the most the native `aac` encoder actually delivers — see the
/// encoder note in monke-worker. A nominal 320k tier produced the same ~223k bytes as this
/// one while claiming otherwise.
///
/// This was a 64/128/224 ladder. The lower two are retired: the player defaults to the
/// highest ready tier and nothing picks a lower one automatically, so each cost a full
/// source download and a full decode — one `transcode_jobs` row per target, each claimed
/// independently — to produce something almost nothing asked for. The sources are kept, so
/// re-adding a tier is this array plus its [`target_column`] arm and a backfill of job rows;
/// the api and the web both render whatever this lists, and neither needs touching.
///
/// The retired tiers' objects were deleted out of the bucket by hand at the same time, which
/// is what lets this list be the only one: `sweep::finalize` derives what to delete from it
/// and consults nothing about how far a transcode ever got, so a name that is not here is a
/// key nothing will ever clean up. Anything writing `aac_64` or `aac_128` objects again would
/// strand them.
pub const TARGETS: [&str; 1] = ["aac_224"];

/// The `audio_files` column flagging this tier as present in the bucket.
///
/// Returns a `&'static str` from a fixed set rather than formatting the target into a
/// column name: the value reaches SQL as an identifier, where it cannot be a bind
/// parameter, so the only safe version is one that can only ever be a literal from this
/// list.
///
/// A retired tier's name returns `None` on purpose. `download_audio` checks the flag this
/// names before serving, so keeping an arm for `aac_64` would let `?tier=aac_64` pass that
/// check on an old row and then 500 on an object that is no longer in the bucket.
pub fn target_column(target: &str) -> Option<&'static str> {
    match target {
        "aac_224" => Some("has_aac_224"),
        _ => None,
    }
}

/// ffmpeg's `-b:a` value: `aac_224` -> `224k`.
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

/// Payloads a worker publishes on a [`progress_key`] channel, beyond a bare 0-100 integer.
///
/// The key itself only ever holds a percentage — it exists so a *polling* reader (list_audio,
/// and the SSE snapshot) can see where a running transcode got to, and it expires on its own.
/// The channel additionally carries these two terminal markers, because a tier going ready or
/// failed is a Postgres write that a live subscriber would otherwise not hear about until it
/// refetched. Constants here so both sides cannot drift on the spelling.
pub const PROGRESS_READY: &str = "ready";
pub const PROGRESS_FAILED: &str = "failed";

/// Redis key *and* pub/sub channel for one in-flight transcode's percentage. The api
/// `PSUBSCRIBE`s `progress:{audio_file_id}:*` to pick up every tier of one file at once.
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
