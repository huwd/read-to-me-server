// SPDX-License-Identifier: AGPL-3.0-or-later
//! Pure domain types for read-to-me: items, the pipeline stages they pass through, and the
//! state machine that governs them (docs/03 §5). No I/O.

use std::fmt;
use std::str::FromStr;

/// Milliseconds since the Unix epoch. Passed in explicitly so time-dependent behaviour
/// (leases, backoff) is deterministic in tests.
pub type Millis = i64;

/// An item's identifier: a UUIDv7, so IDs sort by creation time. Never relied on for secrecy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ItemId(uuid::Uuid);

impl ItemId {
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}

impl Default for ItemId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for ItemId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        uuid::Uuid::parse_str(s).map(Self)
    }
}

/// The owner of an item. Opaque here; maps to an OIDC (`iss`, `sub`) pair elsewhere (ADR-0004).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UserId(pub String);

/// Pipeline stages, in order. Completing the last one publishes the item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    Fetch,
    Extract,
    Script,
    Synthesise,
    Encode,
    Publish,
}

impl Stage {
    pub const ALL: [Stage; 6] = [
        Stage::Fetch,
        Stage::Extract,
        Stage::Script,
        Stage::Synthesise,
        Stage::Encode,
        Stage::Publish,
    ];

    /// The stage after this one, or `None` after `Publish`.
    pub fn next(self) -> Option<Stage> {
        let i = Self::ALL
            .iter()
            .position(|s| *s == self)
            .expect("stage in ALL");
        Self::ALL.get(i + 1).copied()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Fetch => "fetch",
            Stage::Extract => "extract",
            Stage::Script => "script",
            Stage::Synthesise => "synthesise",
            Stage::Encode => "encode",
            Stage::Publish => "publish",
        }
    }
}

impl FromStr for Stage {
    type Err = UnknownValue;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|st| st.as_str() == s)
            .ok_or_else(|| UnknownValue(s.to_string()))
    }
}

/// Why an item failed. User-attributable codes are terminal; others may be retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureCode {
    DestinationNotAllowed,
    RobotsDisallowed,
    SourceTooLarge,
    UnsupportedContentType,
    /// The page answered, but not with the article: not found, gone, forbidden, or a
    /// redirect loop. Retrying won't help.
    SourceUnavailable,
    /// The page couldn't be reached right now: network errors, timeouts, server errors,
    /// rate limiting, or an unreachable robots.txt.
    FetchFailed,
    ExtractionLowConfidence,
    ExtractionTruncated,
    TooLong,
    LanguageUnsupported,
    EngineUnavailable,
    Internal,
}

impl FailureCode {
    pub const ALL: [FailureCode; 12] = [
        FailureCode::DestinationNotAllowed,
        FailureCode::RobotsDisallowed,
        FailureCode::SourceTooLarge,
        FailureCode::UnsupportedContentType,
        FailureCode::SourceUnavailable,
        FailureCode::FetchFailed,
        FailureCode::ExtractionLowConfidence,
        FailureCode::ExtractionTruncated,
        FailureCode::TooLong,
        FailureCode::LanguageUnsupported,
        FailureCode::EngineUnavailable,
        FailureCode::Internal,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            FailureCode::DestinationNotAllowed => "DESTINATION_NOT_ALLOWED",
            FailureCode::RobotsDisallowed => "ROBOTS_DISALLOWED",
            FailureCode::SourceTooLarge => "SOURCE_TOO_LARGE",
            FailureCode::UnsupportedContentType => "UNSUPPORTED_CONTENT_TYPE",
            FailureCode::SourceUnavailable => "SOURCE_UNAVAILABLE",
            FailureCode::FetchFailed => "FETCH_FAILED",
            FailureCode::ExtractionLowConfidence => "EXTRACTION_LOW_CONFIDENCE",
            FailureCode::ExtractionTruncated => "EXTRACTION_TRUNCATED",
            FailureCode::TooLong => "TOO_LONG",
            FailureCode::LanguageUnsupported => "LANGUAGE_UNSUPPORTED",
            FailureCode::EngineUnavailable => "ENGINE_UNAVAILABLE",
            FailureCode::Internal => "INTERNAL",
        }
    }

    /// Transient failures are worth retrying with backoff; the rest fail the item at once.
    pub fn retryable(self) -> bool {
        matches!(
            self,
            FailureCode::FetchFailed | FailureCode::EngineUnavailable | FailureCode::Internal
        )
    }
}

impl FromStr for FailureCode {
    type Err = UnknownValue;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .iter()
            .copied()
            .find(|c| c.as_str() == s)
            .ok_or_else(|| UnknownValue(s.to_string()))
    }
}

/// A stored value that doesn't match any known variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownValue(pub String);

impl fmt::Display for UnknownValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown value {:?}", self.0)
    }
}

impl std::error::Error for UnknownValue {}

/// Where an item is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemState {
    /// Waiting for (or running) `next`.
    Pending {
        next: Stage,
    },
    Published,
    Failed {
        stage: Stage,
        code: FailureCode,
    },
}

impl ItemState {
    /// A newly submitted item.
    pub fn submitted() -> Self {
        ItemState::Pending { next: Stage::Fetch }
    }

    /// Record `stage` as completed. Only the stage the item is waiting for can complete.
    pub fn complete(self, stage: Stage) -> Result<ItemState, TransitionError> {
        match self {
            ItemState::Pending { next } if next == stage => Ok(match stage.next() {
                Some(next) => ItemState::Pending { next },
                None => ItemState::Published,
            }),
            other => Err(TransitionError { from: other, stage }),
        }
    }

    /// Record `stage` as failed terminally. Only the stage the item is waiting for can fail.
    pub fn fail(self, stage: Stage, code: FailureCode) -> Result<ItemState, TransitionError> {
        match self {
            ItemState::Pending { next } if next == stage => Ok(ItemState::Failed { stage, code }),
            other => Err(TransitionError { from: other, stage }),
        }
    }

    pub fn is_terminal(self) -> bool {
        !matches!(self, ItemState::Pending { .. })
    }
}

/// An attempted transition the state machine does not allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionError {
    pub from: ItemState,
    pub stage: Stage,
}

impl fmt::Display for TransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "stage {:?} cannot change an item in state {:?}",
            self.stage, self.from
        )
    }
}

impl std::error::Error for TransitionError {}

/// The address of an immutable blob: a BLAKE3 hash as 64 lowercase hex characters
/// (ADR-0008). Never a capability: keys don't appear in public URLs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlobKey(String);

impl BlobKey {
    pub fn from_hash(hash: [u8; 32]) -> Self {
        Self(hash.iter().map(|b| format!("{b:02x}")).collect())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BlobKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for BlobKey {
    type Err = UnknownValue;
    /// Only well-formed keys parse, so a key can safely become a file name.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let valid = s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        valid
            .then(|| Self(s.to_string()))
            .ok_or_else(|| UnknownValue(s.to_string()))
    }
}

/// What kind of document a source is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    Html,
    PlainText,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceKind::Html => "html",
            SourceKind::PlainText => "text",
        }
    }
}

impl FromStr for SourceKind {
    type Err = UnknownValue;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "html" => Ok(SourceKind::Html),
            "text" => Ok(SourceKind::PlainText),
            other => Err(UnknownValue(other.to_string())),
        }
    }
}

/// The fetched document an item is made from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub key: BlobKey,
    /// Where the document was found, after redirects.
    pub final_url: String,
    pub kind: SourceKind,
    /// The declared character set, if any.
    pub charset: Option<String>,
    pub size: u64,
}

/// The extracted article an item narrates (ADR-0006, FR-EXT-04).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArticleRecord {
    pub key: BlobKey,
    /// `id@version` of the extractor that produced it.
    pub extractor: String,
    pub title: String,
    /// True when the page had no title and one was derived.
    pub title_derived: bool,
    pub words: u64,
}

/// The narration script an item will be synthesised from (FR-TTS-01).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptRecord {
    pub key: BlobKey,
    /// Words to be spoken, including the title and intro.
    pub words: u64,
}

/// The synthesised speech for an item: a plan of audio segments and the pauses between
/// them, which encoding assembles into one episode (FR-TTS-04).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechRecord {
    pub plan_key: BlobKey,
    /// Total length, segments and pauses together.
    pub duration_ms: u64,
    pub segments: u64,
}

/// The encoded episode: one MP3 (ADR-0009).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EpisodeRecord {
    pub key: BlobKey,
    pub bytes: u64,
    pub duration_ms: u64,
}

/// A submitted article and where it is in the pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: ItemId,
    pub user: UserId,
    pub source_url: String,
    pub state: ItemState,
    pub failure_message: Option<String>,
    /// Set once the fetch stage completes.
    pub source: Option<Source>,
    /// Set once the extract stage completes.
    pub article: Option<ArticleRecord>,
    /// Set once the script stage completes.
    pub script: Option<ScriptRecord>,
    /// Set once the synthesise stage completes.
    pub speech: Option<SpeechRecord>,
    /// Set once the encode stage completes.
    pub episode: Option<EpisodeRecord>,
    /// When the item was published to the feed.
    pub published_at: Option<Millis>,
    pub created_at: Millis,
    pub updated_at: Millis,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_publishes_after_every_stage_in_order() {
        let mut state = ItemState::submitted();
        for stage in Stage::ALL {
            state = state.complete(stage).unwrap();
        }
        assert_eq!(state, ItemState::Published);
    }

    #[test]
    fn codes_and_stages_round_trip_through_strings() {
        for s in Stage::ALL {
            assert_eq!(s.as_str().parse::<Stage>().unwrap(), s);
        }
        for c in FailureCode::ALL {
            assert_eq!(c.as_str().parse::<FailureCode>().unwrap(), c);
        }
        assert!("nonsense".parse::<Stage>().is_err());
    }

    #[test]
    fn blob_keys_are_strict_hex() {
        let key = BlobKey::from_hash([0xab; 32]);
        assert_eq!(key.as_str(), "ab".repeat(32));
        assert_eq!(key.as_str().parse::<BlobKey>().unwrap(), key);
        for bad in [
            "",
            "AB".repeat(32).as_str(),
            "../etc/passwd",
            &"a".repeat(63),
            &"g".repeat(64),
        ] {
            assert!(bad.parse::<BlobKey>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn item_ids_sort_by_creation() {
        let a = ItemId::new();
        let b = ItemId::new();
        assert!(a < b);
        assert_eq!(a.to_string().parse::<ItemId>().unwrap(), a);
    }
}
