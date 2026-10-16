// SPDX-License-Identifier: AGPL-3.0-or-later
//! The encode and publish stages with real ffmpeg, checked with ffprobe and ffmpeg's
//! own loudness meter. Skipped where ffmpeg isn't installed, except on CI (`CI` set),
//! where a missing ffmpeg fails the test so it can't silently not run.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use rtm_article::Article;
use rtm_blobs::{BlobKind, BlobStore, segment_key};
use rtm_domain::{
    ArticleRecord, BlobKey, Item, ItemState, ScriptRecord, Source, SourceKind, SpeechRecord, Stage,
    UserId,
};
use rtm_pipeline::{EncodeConfig, EncodeStage, PublishStage, Turn, Worker};
use rtm_ports::{ItemRepository, JobQueue, StageOutput};
use rtm_speech::plan::{PlannedSegment, SpeechPlan};
use rtm_speech::{FakeEngine, SpeechEngine};
use rtm_store::{QueuePolicy, Store};

fn tool(name: &str) -> Option<PathBuf> {
    let found = std::env::var("PATH")
        .ok()?
        .split(':')
        .map(|d| PathBuf::from(d).join(name))
        .find(|p| p.is_file());
    if found.is_none() && std::env::var_os("CI").is_some() {
        panic!("{name} is required on CI");
    }
    found
}

fn owner() -> UserId {
    UserId("owner".into())
}

struct Setup {
    store: Arc<Store>,
    blobs: BlobStore,
    ffmpeg: PathBuf,
    _dir: tempfile::TempDir,
}

impl Setup {
    async fn new() -> Option<Setup> {
        let Some(ffmpeg) = tool("ffmpeg") else {
            eprintln!("skipping: no ffmpeg on PATH");
            return None;
        };
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            Store::open(&dir.path().join("rtm.db"), QueuePolicy::default())
                .await
                .unwrap(),
        );
        Some(Setup {
            store,
            blobs: BlobStore::new(dir.path().join("blobs")),
            ffmpeg,
            _dir: dir,
        })
    }

    /// An item synthesised with the fake engine and waiting to be encoded.
    async fn synthesised(&self, sentences: &[&str]) -> (Item, SpeechPlan) {
        let item = self
            .store
            .submit(&owner(), "https://waterways.example/canals", 0)
            .await
            .unwrap();
        let article = Article {
            title: "The Quiet Engineering of Canals".into(),
            byline: Some("Ada Lovelock".into()),
            site_name: None,
            published: None,
            canonical_url: None,
            lang: Some("en".into()),
            blocks: vec![],
        };
        let article_key = BlobKey::from_hash([2; 32]);
        self.blobs
            .put(
                BlobKind::Article,
                &article_key,
                serde_json::to_vec(&article).unwrap(),
            )
            .await
            .unwrap();
        let mut segments = Vec::new();
        for text in sentences {
            let pcm = FakeEngine.synthesize(text, "bf_emma").unwrap();
            let key = segment_key(text, "en", "fake@1/bf_emma");
            self.blobs
                .put(BlobKind::Segment, &key, pcm.to_wav())
                .await
                .unwrap();
            segments.push(PlannedSegment {
                key: key.to_string(),
                duration_ms: pcm.duration_ms(),
                pause_after_ms: 400,
            });
        }
        let plan = SpeechPlan {
            sample_rate: 24_000,
            segments,
        };
        let plan_key = BlobKey::from_hash([4; 32]);
        self.blobs
            .put(
                BlobKind::Plan,
                &plan_key,
                serde_json::to_vec(&plan).unwrap(),
            )
            .await
            .unwrap();
        let outputs = [
            (
                Stage::Fetch,
                StageOutput::Source(Source {
                    key: BlobKey::from_hash([1; 32]),
                    final_url: "https://waterways.example/canals".into(),
                    kind: SourceKind::Html,
                    charset: None,
                    size: 1,
                }),
            ),
            (
                Stage::Extract,
                StageOutput::Article(ArticleRecord {
                    key: article_key,
                    extractor: "generic-html@1".into(),
                    title: article.title.clone(),
                    title_derived: false,
                    words: 300,
                }),
            ),
            (
                Stage::Script,
                StageOutput::Script(ScriptRecord {
                    key: BlobKey::from_hash([3; 32]),
                    words: 300,
                }),
            ),
            (
                Stage::Synthesise,
                StageOutput::Speech(SpeechRecord {
                    plan_key,
                    duration_ms: plan.duration_ms(),
                    segments: plan.segments.len() as u64,
                }),
            ),
        ];
        for (n, (stage, output)) in outputs.into_iter().enumerate() {
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
        (
            self.store.get(&owner(), item.id).await.unwrap().unwrap(),
            plan,
        )
    }

    fn encoder(&self) -> Worker<Store> {
        let stage = EncodeStage::new(
            self.blobs.clone(),
            EncodeConfig {
                ffmpeg: self.ffmpeg.clone(),
                timeout: Duration::from_secs(120),
            },
        );
        Worker::new("e", Arc::clone(&self.store), Arc::new(stage))
    }
}

fn probe(path: &std::path::Path) -> serde_json::Value {
    let ffprobe = tool("ffprobe").expect("ffprobe alongside ffmpeg");
    let out = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output()
        .unwrap();
    serde_json::from_slice(&out.stdout).unwrap()
}

/// Integrated loudness in LUFS, from ffmpeg's EBU R128 meter.
fn loudness(ffmpeg: &std::path::Path, path: &std::path::Path) -> f64 {
    let out = Command::new(ffmpeg)
        .args(["-nostdin", "-hide_banner", "-i"])
        .arg(path)
        .args(["-af", "ebur128", "-f", "null", "-"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let summary = &stderr[stderr.rfind("Summary:").expect("ebur128 summary")..];
    let line = summary
        .lines()
        .find(|l| l.trim_start().starts_with("I:"))
        .unwrap();
    line.split_whitespace().nth(1).unwrap().parse().unwrap()
}

#[tokio::test]
async fn speech_becomes_a_normalised_tagged_mp3() {
    let Some(s) = Setup::new().await else { return };
    let sentences = [
        "Canals are among the oldest pieces of engineered infrastructure still in use.",
        "Their builders worked without modern surveying instruments.",
        "Many of their routes remain hard to improve on today.",
    ];
    let (item, plan) = s.synthesised(&sentences).await;
    assert_eq!(
        s.encoder().run_once().await.unwrap(),
        Turn::Completed(Stage::Encode)
    );

    let item = s.store.get(&owner(), item.id).await.unwrap().unwrap();
    assert_eq!(
        item.state,
        ItemState::Pending {
            next: Stage::Publish
        }
    );
    let episode = item.episode.clone().expect("episode recorded");
    let path = s.blobs.local_path(BlobKind::Episode, &episode.key);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), episode.bytes);
    assert_eq!(episode.duration_ms, plan.duration_ms());

    // ADR-0009: MP3, 44.1 kHz, mono, 64 kbps CBR.
    let info = probe(&path);
    let stream = &info["streams"][0];
    assert_eq!(stream["codec_name"], "mp3");
    assert_eq!(stream["sample_rate"], "44100");
    assert_eq!(stream["channels"], 1);
    assert_eq!(stream["bit_rate"], "64000");
    let seconds: f64 = info["format"]["duration"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let expected = plan.duration_ms() as f64 / 1000.0;
    assert!(
        (seconds - expected).abs() < 0.5,
        "{seconds} s, expected {expected} s"
    );
    let tags = &info["format"]["tags"];
    assert_eq!(tags["title"], "The Quiet Engineering of Canals");
    assert_eq!(tags["artist"], "Ada Lovelock");
    assert_eq!(tags["comment"], "https://waterways.example/canals");

    // −16 LUFS integrated, within the meter's tolerance on short programme material.
    let lufs = loudness(&s.ffmpeg, &path);
    assert!((-17.5..=-14.5).contains(&lufs), "{lufs} LUFS");

    // Nothing left behind in scratch space.
    let scratch: Vec<_> = std::fs::read_dir(s._dir.path().join("blobs/tmp"))
        .unwrap()
        .collect();
    assert!(scratch.is_empty(), "{scratch:?}");

    // Publishing stamps the time and finishes the item.
    let publisher = Worker::new("p", Arc::clone(&s.store), Arc::new(PublishStage));
    assert_eq!(
        publisher.run_once().await.unwrap(),
        Turn::Completed(Stage::Publish)
    );
    let item = s.store.get(&owner(), item.id).await.unwrap().unwrap();
    assert_eq!(item.state, ItemState::Published);
    assert!(item.published_at.is_some());
}

#[tokio::test]
async fn an_episode_already_encoded_is_reused() {
    let Some(s) = Setup::new().await else { return };
    let (first, _) = s
        .synthesised(&["One sentence of speech for the test."])
        .await;
    s.encoder().run_once().await.unwrap();
    let first = s.store.get(&owner(), first.id).await.unwrap().unwrap();
    let path = s
        .blobs
        .local_path(BlobKind::Episode, &first.episode.as_ref().unwrap().key);
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();

    // A second item with the same plan, article and tags gets the same episode.
    let (second, _) = s
        .synthesised(&["One sentence of speech for the test."])
        .await;
    s.encoder().run_once().await.unwrap();
    let second = s.store.get(&owner(), second.id).await.unwrap().unwrap();
    assert_eq!(second.episode, first.episode);
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        modified
    );
}

#[tokio::test]
async fn a_missing_encoder_is_retried() {
    let Some(s) = Setup::new().await else { return };
    let (item, _) = s.synthesised(&["Some speech."]).await;
    let stage = EncodeStage::new(
        s.blobs.clone(),
        EncodeConfig {
            ffmpeg: "/nonexistent/ffmpeg".into(),
            timeout: Duration::from_secs(10),
        },
    );
    let worker = Worker::new("e", Arc::clone(&s.store), Arc::new(stage));
    assert_eq!(
        worker.run_once().await.unwrap(),
        Turn::Retrying(Stage::Encode, rtm_domain::FailureCode::EngineUnavailable)
    );
    let item = s.store.get(&owner(), item.id).await.unwrap().unwrap();
    assert_eq!(
        item.state,
        ItemState::Pending {
            next: Stage::Encode
        }
    );
}
