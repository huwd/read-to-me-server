// SPDX-License-Identifier: AGPL-3.0-or-later
//! The synthesise stage against the real `rtm tts` HTTP service, running the fake
//! engine on a local port, with real SQLite and blobs.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use rtm_blobs::{BlobKind, BlobStore, segment_key};
use rtm_domain::{
    ArticleRecord, BlobKey, FailureCode, Item, ItemState, ScriptRecord, Source, SourceKind, Stage,
    UserId,
};
use rtm_narrate::{Script, Utterance};
use rtm_pipeline::{SynthesiseConfig, SynthesiseStage, Turn, Worker};
use rtm_ports::{ItemRepository, JobQueue, StageOutput};
use rtm_speech::client::TtsClient;
use rtm_speech::plan::SpeechPlan;
use rtm_speech::{FakeEngine, Pcm, SpeechEngine, SpeechError, Voice};
use rtm_store::{QueuePolicy, Store};

/// The fake engine, counting syntheses, and changing its fingerprint after
/// `upgrade_after` of them, as if a new model had been deployed.
struct Counting {
    calls: AtomicUsize,
    upgrade_after: AtomicUsize,
}

impl Default for Counting {
    fn default() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            upgrade_after: AtomicUsize::new(usize::MAX),
        }
    }
}

impl SpeechEngine for Counting {
    fn id(&self) -> &str {
        "fake"
    }
    fn fingerprint(&self, voice: &str) -> Option<String> {
        let upgraded =
            self.calls.load(Ordering::SeqCst) >= self.upgrade_after.load(Ordering::SeqCst);
        let version = if upgraded { 2 } else { 1 };
        FakeEngine
            .fingerprint(voice)
            .map(|f| format!("{f}#{version}"))
    }
    fn voices(&self) -> Vec<Voice> {
        FakeEngine.voices()
    }
    fn sample_rate(&self) -> u32 {
        FakeEngine.sample_rate()
    }
    fn synthesize(&self, text: &str, voice: &str) -> Result<Pcm, SpeechError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        FakeEngine.synthesize(text, voice)
    }
}

async fn start_tts(engine: Arc<Counting>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, rtm_tts::router(engine))
            .await
            .unwrap();
    });
    format!("http://{addr}")
}

fn owner() -> UserId {
    UserId("owner".into())
}

fn script() -> Script {
    let long = "The canal climbs the hill in a flight of locks. ".repeat(12);
    Script {
        lang: "en".into(),
        utterances: vec![
            Utterance {
                text: "The Quiet Engineering of Canals".into(),
                pause_after_ms: 900,
            },
            Utterance {
                text: long.trim().into(),
                pause_after_ms: 500,
            },
            Utterance {
                text: "Locks are clever.".into(),
                pause_after_ms: 0,
            },
        ],
    }
}

struct Setup {
    store: Arc<Store>,
    blobs: BlobStore,
    engine: Arc<Counting>,
    url: String,
    _dir: tempfile::TempDir,
}

impl Setup {
    async fn new() -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            Store::open(&dir.path().join("rtm.db"), QueuePolicy::default())
                .await
                .unwrap(),
        );
        let engine = Arc::new(Counting::default());
        let url = start_tts(Arc::clone(&engine)).await;
        Setup {
            store,
            blobs: BlobStore::new(dir.path().join("blobs")),
            engine,
            url,
            _dir: dir,
        }
    }

    /// An item scripted with `script` and waiting for synthesis.
    async fn scripted(&self, script: &Script) -> Item {
        let item = self
            .store
            .submit(&owner(), "https://example.test/a", 0)
            .await
            .unwrap();
        let outputs = [
            StageOutput::Source(Source {
                key: BlobKey::from_hash([1; 32]),
                final_url: "https://example.test/a".into(),
                kind: SourceKind::Html,
                charset: None,
                size: 1,
            }),
            StageOutput::Article(ArticleRecord {
                key: BlobKey::from_hash([2; 32]),
                extractor: "generic-html@1".into(),
                title: "Canals".into(),
                title_derived: false,
                words: 100,
            }),
        ];
        for (n, (stage, output)) in [Stage::Fetch, Stage::Extract]
            .into_iter()
            .zip(outputs)
            .enumerate()
        {
            let at = 1 + 2 * n as i64;
            let job = self
                .store
                .claim("w", &[stage], at, 60_000)
                .await
                .unwrap()
                .unwrap();
            self.store
                .complete(&job, "w", output, at + 1)
                .await
                .unwrap();
        }
        let key = BlobKey::from_hash([3; 32]);
        self.blobs
            .put(BlobKind::Script, &key, serde_json::to_vec(script).unwrap())
            .await
            .unwrap();
        let job = self
            .store
            .claim("w", &[Stage::Script], 5, 60_000)
            .await
            .unwrap()
            .unwrap();
        let output = StageOutput::Script(ScriptRecord {
            key,
            words: script.word_count() as u64,
        });
        self.store.complete(&job, "w", output, 6).await.unwrap();
        self.store.get(&owner(), item.id).await.unwrap().unwrap()
    }

    fn worker_at(&self, url: &str, voice: &str) -> Worker<Store> {
        let client = TtsClient::new(url, Duration::from_secs(10)).unwrap();
        let config = SynthesiseConfig {
            voice: voice.into(),
            piece_chars: 200,
            ..SynthesiseConfig::default()
        };
        let stage = SynthesiseStage::new(client, self.blobs.clone(), config);
        Worker::new("t", Arc::clone(&self.store), Arc::new(stage))
    }

    fn worker(&self) -> Worker<Store> {
        self.worker_at(&self.url, "bf_emma")
    }

    async fn reload(&self, item: &Item) -> Item {
        self.store.get(&owner(), item.id).await.unwrap().unwrap()
    }

    async fn plan(&self, item: &Item) -> SpeechPlan {
        let key = &item.speech.as_ref().unwrap().plan_key;
        serde_json::from_slice(&self.blobs.get(BlobKind::Plan, key).await.unwrap()).unwrap()
    }
}

/// The number of different segments a plan uses.
fn distinct(plan: &SpeechPlan) -> usize {
    plan.segments
        .iter()
        .map(|p| &p.key)
        .collect::<std::collections::HashSet<_>>()
        .len()
}

#[tokio::test]
async fn a_script_becomes_cached_segments_and_a_plan() {
    let s = Setup::new().await;
    let item = s.scripted(&script()).await;
    assert_eq!(
        s.worker().run_once().await.unwrap(),
        Turn::Completed(Stage::Synthesise)
    );

    let item = s.reload(&item).await;
    assert_eq!(
        item.state,
        ItemState::Pending {
            next: Stage::Encode
        }
    );
    let plan = s.plan(&item).await;
    let record = item.speech.as_ref().unwrap();
    assert_eq!(plan.sample_rate, 24_000);
    // The title, the long paragraph in 200-character pieces, and the last line.
    assert!(plan.segments.len() >= 5, "{}", plan.segments.len());
    assert_eq!(record.segments, plan.segments.len() as u64);
    assert_eq!(record.duration_ms, plan.duration_ms());
    // Identical pieces (the paragraph repeats a sentence) are synthesised once and the
    // cached audio reused, even within one item.
    assert_eq!(s.engine.calls.load(Ordering::SeqCst), distinct(&plan));
    assert!(
        distinct(&plan) < plan.segments.len(),
        "the test expects repeats"
    );

    // Pauses: the utterance's after its last piece, the short gap between pieces.
    let pauses: Vec<u32> = plan.segments.iter().map(|p| p.pause_after_ms).collect();
    assert_eq!(pauses[0], 900);
    assert!(
        pauses[1..pauses.len() - 2].iter().all(|p| *p == 150),
        "{pauses:?}"
    );
    assert_eq!(pauses[pauses.len() - 2], 500);
    assert_eq!(*pauses.last().unwrap(), 0);

    // Every segment is stored as audio of the length the plan says.
    for planned in &plan.segments {
        let key: BlobKey = planned.key.parse().unwrap();
        let pcm = Pcm::from_wav(&s.blobs.get(BlobKind::Segment, &key).await.unwrap()).unwrap();
        assert_eq!(pcm.duration_ms(), planned.duration_ms);
    }
    let first: BlobKey = plan.segments[0].key.parse().unwrap();
    assert_eq!(
        first,
        segment_key("The Quiet Engineering of Canals", "en", "fake@1/bf_emma#1")
    );
}

/// FR-TTS-04: a crash mid-article resumes rather than restarting.
#[tokio::test]
async fn synthesis_resumes_from_cached_segments() {
    let s = Setup::new().await;
    // A previous attempt got as far as the title before crashing.
    let title = "The Quiet Engineering of Canals";
    let audio = FakeEngine.synthesize(title, "bf_emma").unwrap();
    let key = segment_key(title, "en", "fake@1/bf_emma#1");
    s.blobs
        .put(BlobKind::Segment, &key, audio.to_wav())
        .await
        .unwrap();

    let item = s.scripted(&script()).await;
    s.worker().run_once().await.unwrap();
    let plan = s.plan(&s.reload(&item).await).await;
    assert_eq!(s.engine.calls.load(Ordering::SeqCst), distinct(&plan) - 1);

    // A second item with the same script needs no synthesis at all.
    let before = s.engine.calls.load(Ordering::SeqCst);
    let again = s.scripted(&script()).await;
    s.worker().run_once().await.unwrap();
    assert_eq!(s.engine.calls.load(Ordering::SeqCst), before);
    assert_eq!(s.plan(&s.reload(&again).await).await, plan);
}

#[tokio::test]
async fn an_unreachable_engine_is_retried() {
    let s = Setup::new().await;
    let item = s.scripted(&script()).await;
    // Nothing listens on this port.
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let turn = s
        .worker_at(&format!("http://{closed}"), "bf_emma")
        .run_once()
        .await
        .unwrap();
    assert_eq!(
        turn,
        Turn::Retrying(Stage::Synthesise, FailureCode::EngineUnavailable)
    );
    assert_eq!(
        s.reload(&item).await.state,
        ItemState::Pending {
            next: Stage::Synthesise
        }
    );
}

#[tokio::test]
async fn a_voice_the_engine_lacks_is_retried() {
    let s = Setup::new().await;
    s.scripted(&script()).await;
    let turn = s.worker_at(&s.url, "nobody").run_once().await.unwrap();
    assert_eq!(
        turn,
        Turn::Retrying(Stage::Synthesise, FailureCode::EngineUnavailable)
    );
    assert_eq!(s.engine.calls.load(Ordering::SeqCst), 0);
}

/// If the engine is upgraded mid-job, its new audio must not be cached under the old
/// fingerprint's key: the job is retried and picks up the new fingerprint.
#[tokio::test]
async fn an_engine_change_mid_job_is_retried_without_caching() {
    let s = Setup::new().await;
    let item = s.scripted(&script()).await;
    // The first piece is made by version 1; the service labels the second version 2.
    s.engine.upgrade_after.store(1, Ordering::SeqCst);
    let turn = s.worker().run_once().await.unwrap();
    assert_eq!(
        turn,
        Turn::Retrying(Stage::Synthesise, FailureCode::EngineUnavailable)
    );
    assert_eq!(s.reload(&item).await.speech, None);

    // Only the first piece was cached, under version 1's key.
    let second_piece = &script().utterances[1].text;
    let first_of_long = rtm_speech::segment::pieces(second_piece, 200)[0].clone();
    for version in [1, 2] {
        let key = segment_key(&first_of_long, "en", &format!("fake@1/bf_emma#{version}"));
        assert!(
            !s.blobs.exists(BlobKind::Segment, &key).await.unwrap(),
            "v{version} cached"
        );
    }
    let title_v1 = segment_key("The Quiet Engineering of Canals", "en", "fake@1/bf_emma#1");
    assert!(s.blobs.exists(BlobKind::Segment, &title_v1).await.unwrap());
}
