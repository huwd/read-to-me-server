// SPDX-License-Identifier: AGPL-3.0-or-later
//! The script stage on a real store and blob store.

use std::sync::Arc;

use rtm_article::{Article, Block};
use rtm_blobs::{BlobKind, BlobStore};
use rtm_domain::{
    ArticleRecord, BlobKey, FailureCode, Item, ItemState, Source, SourceKind, Stage, UserId,
};
use rtm_narrate::{Options, Script};
use rtm_pipeline::{ScriptStage, Turn, Worker};
use rtm_ports::{ItemRepository, JobQueue, StageOutput};
use rtm_store::{QueuePolicy, Store};

struct Setup {
    store: Arc<Store>,
    blobs: BlobStore,
    _dir: tempfile::TempDir,
}

fn owner() -> UserId {
    UserId("owner".into())
}

fn canal_article(lang: Option<&str>, paragraphs: usize) -> Article {
    Article {
        title: "The Quiet Engineering of Canals".into(),
        byline: Some("Ada Lovelock".into()),
        site_name: None,
        published: Some("2026-03-14".into()),
        canonical_url: None,
        lang: lang.map(Into::into),
        blocks: (0..paragraphs)
            .map(|n| Block::Paragraph {
                text: format!("Paragraph {n} about locks and levels."),
            })
            .collect(),
    }
}

impl Setup {
    async fn new() -> Setup {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(
            Store::open(&dir.path().join("rtm.db"), QueuePolicy::default())
                .await
                .unwrap(),
        );
        let blobs = BlobStore::new(dir.path().join("blobs"));
        Setup {
            store,
            blobs,
            _dir: dir,
        }
    }

    /// An item extracted as `article` (with `words` recorded) and waiting for its
    /// script. `store_blob: false` leaves the article blob missing.
    async fn extracted(&self, article: &Article, words: u64, store_blob: bool) -> Item {
        let item = self
            .store
            .submit(&owner(), "https://example.test/a", 0)
            .await
            .unwrap();
        let job = self
            .store
            .claim("w", &[Stage::Fetch], 1, 60_000)
            .await
            .unwrap()
            .unwrap();
        let source = Source {
            key: BlobKey::from_hash([1; 32]),
            final_url: "https://example.test/a".into(),
            kind: SourceKind::Html,
            charset: None,
            size: 1,
        };
        self.store
            .complete(&job, "w", StageOutput::Source(source), 2)
            .await
            .unwrap();
        let key = BlobKey::from_hash([2; 32]);
        if store_blob {
            let json = serde_json::to_vec(article).unwrap();
            self.blobs.put(BlobKind::Article, &key, json).await.unwrap();
        }
        let job = self
            .store
            .claim("w", &[Stage::Extract], 3, 60_000)
            .await
            .unwrap()
            .unwrap();
        let record = ArticleRecord {
            key,
            extractor: "generic-html@1".into(),
            title: article.title.clone(),
            title_derived: false,
            words,
        };
        self.store
            .complete(&job, "w", StageOutput::Article(record), 4)
            .await
            .unwrap();
        self.store.get(&owner(), item.id).await.unwrap().unwrap()
    }

    fn worker(&self, max_words: u64) -> Worker<Store> {
        let stage = ScriptStage::new(self.blobs.clone(), Options::default(), max_words);
        Worker::new("s", Arc::clone(&self.store), Arc::new(stage))
    }

    async fn reload(&self, item: &Item) -> Item {
        self.store.get(&owner(), item.id).await.unwrap().unwrap()
    }
}

#[tokio::test]
async fn an_article_is_narrated_and_stored() {
    let s = Setup::new().await;
    let article = canal_article(Some("en-GB"), 3);
    let item = s.extracted(&article, 30, true).await;
    assert_eq!(
        s.worker(30_000).run_once().await.unwrap(),
        Turn::Completed(Stage::Script)
    );

    let item = s.reload(&item).await;
    assert_eq!(
        item.state,
        ItemState::Pending {
            next: Stage::Synthesise
        }
    );
    let record = item.script.expect("script recorded");
    let script: Script =
        serde_json::from_slice(&s.blobs.get(BlobKind::Script, &record.key).await.unwrap()).unwrap();
    assert_eq!(script.lang, "en");
    assert_eq!(script.utterances[0].text, "The Quiet Engineering of Canals");
    assert_eq!(
        script.utterances[1].text,
        "By Ada Lovelock. Published 14 March 2026."
    );
    assert_eq!(script.utterances.len(), 2 + 3);
    assert_eq!(record.words, script.word_count() as u64);
}

#[tokio::test]
async fn the_same_article_and_options_give_the_same_script_key() {
    let s = Setup::new().await;
    let article = canal_article(None, 2);
    let a = s.extracted(&article, 20, true).await;
    let b = s.extracted(&article, 20, true).await;
    let worker = s.worker(30_000);
    worker.run_once().await.unwrap();
    worker.run_once().await.unwrap();
    let (a, b) = (s.reload(&a).await, s.reload(&b).await);
    assert_eq!(a.script.unwrap().key, b.script.unwrap().key);
}

// FR-TTS-06
#[tokio::test]
async fn articles_over_the_limit_are_too_long() {
    let s = Setup::new().await;
    let item = s.extracted(&canal_article(None, 1), 30_001, true).await;
    assert_eq!(
        s.worker(30_000).run_once().await.unwrap(),
        Turn::Failed(Stage::Script, FailureCode::TooLong)
    );
    let message = s.reload(&item).await.failure_message.unwrap();
    assert!(
        message.contains("30001") && message.contains("30000"),
        "{message}"
    );
}

// FR-TTS-07
#[tokio::test]
async fn other_languages_are_unsupported() {
    let s = Setup::new().await;
    let item = s
        .extracted(&canal_article(Some("fr-FR"), 1), 10, true)
        .await;
    assert_eq!(
        s.worker(30_000).run_once().await.unwrap(),
        Turn::Failed(Stage::Script, FailureCode::LanguageUnsupported)
    );
    assert!(
        s.reload(&item)
            .await
            .failure_message
            .unwrap()
            .contains("fr-FR")
    );
}

#[tokio::test]
async fn a_missing_article_blob_is_retried() {
    let s = Setup::new().await;
    let item = s.extracted(&canal_article(None, 1), 10, false).await;
    assert_eq!(
        s.worker(30_000).run_once().await.unwrap(),
        Turn::Retrying(Stage::Script, FailureCode::Internal)
    );
    assert_eq!(
        s.reload(&item).await.state,
        ItemState::Pending {
            next: Stage::Script
        }
    );
}
