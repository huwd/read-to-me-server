// SPDX-License-Identifier: AGPL-3.0-or-later
//! The encode stage (ADR-0009): assemble the speech plan's segments and pauses into one
//! recording, normalise its loudness and encode it as the episode MP3.
//!
//! ffmpeg runs as a subprocess with a fixed argument list, no shell, a cleared
//! environment, only local files allowed as inputs, and a time limit (docs/06 §3.2).
//! Its inputs are our own audio, never user files. Episodes can be hours long, so audio
//! streams through scratch files on the blob store's filesystem and the finished MP3 is
//! moved into place rather than copied through memory.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use rtm_article::Article;
use rtm_blobs::{BlobKind, BlobStore, episode_key};
use rtm_domain::{EpisodeRecord, FailureCode, Item, Stage};
use rtm_ports::StageOutput;
use rtm_speech::Pcm;
use rtm_speech::plan::SpeechPlan;
use serde::Serialize;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::OnceCell;

use crate::{StageFailure, StageFuture, StageRunner};

/// Bump on any change to how episodes are encoded: it's part of the episode key.
pub const ENCODER_VERSION: u32 = 1;

/// ADR-0009: −16 LUFS integrated, −1 dBTP true peak.
const LOUDNORM: &str = "loudnorm=I=-16:TP=-1:LRA=11";

/// The longest tag value written, in characters.
const MAX_TAG_CHARS: usize = 300;

#[derive(Debug, Clone)]
pub struct EncodeConfig {
    /// The ffmpeg binary, by absolute path (the environment is cleared).
    pub ffmpeg: PathBuf,
    /// For each ffmpeg pass; three hours of speech takes a minute or two.
    pub timeout: Duration,
}

pub struct EncodeStage {
    blobs: BlobStore,
    config: EncodeConfig,
    /// "ffmpeg version …", read once.
    ffmpeg_version: OnceCell<String>,
}

fn internal(message: &str) -> StageFailure {
    StageFailure {
        code: FailureCode::Internal,
        message: message.into(),
    }
}

/// The ID3 tags written into the episode (ADR-0009).
#[derive(Debug, Clone, Serialize)]
struct Tags {
    title: String,
    artist: String,
    album: String,
    comment: String,
}

/// A tag value from untrusted article text: no control characters, bounded length.
fn tag(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_TAG_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

/// The JSON object `loudnorm` prints at the end of its analysis pass.
fn measured(stderr: &str) -> Option<serde_json::Value> {
    let start = stderr.rfind("{\n")?;
    let end = stderr[start..].find('}')? + start + 1;
    serde_json::from_str(&stderr[start..end]).ok()
}

impl EncodeStage {
    pub fn new(blobs: BlobStore, config: EncodeConfig) -> Self {
        Self {
            blobs,
            config,
            ffmpeg_version: OnceCell::new(),
        }
    }

    /// Run ffmpeg with `args`, returning its stderr on success.
    async fn ffmpeg(&self, args: &[String]) -> Result<String, StageFailure> {
        let mut child = Command::new(&self.config.ffmpeg)
            .args(["-nostdin", "-hide_banner", "-protocol_whitelist", "file"])
            .args(args)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                tracing::error!(error = %e, ffmpeg = %self.config.ffmpeg.display(), "could not start ffmpeg");
                StageFailure {
                    code: FailureCode::EngineUnavailable,
                    message: "The audio encoder isn't available; it will be retried.".into(),
                }
            })?;
        let mut stderr = child.stderr.take().expect("piped");
        let run = async {
            let mut text = String::new();
            // loudnorm's report is small; cap what's kept from a chatty ffmpeg.
            let _ = (&mut stderr)
                .take(4 * 1024 * 1024)
                .read_to_string(&mut text)
                .await;
            (child.wait().await, text)
        };
        let Ok((status, text)) = tokio::time::timeout(self.config.timeout, run).await else {
            return Err(internal("Encoding took too long; it will be retried."));
        };
        match status {
            Ok(s) if s.success() => Ok(text),
            other => {
                let tail: String = text.lines().rev().take(5).collect::<Vec<_>>().join(" | ");
                tracing::error!(status = ?other, stderr = %tail, "ffmpeg failed");
                Err(internal("Encoding failed; it will be retried."))
            }
        }
    }

    /// The encoder's fingerprint: ffmpeg's version and our encoding parameters.
    async fn fingerprint(&self) -> Result<String, StageFailure> {
        let version = self
            .ffmpeg_version
            .get_or_try_init(|| async {
                let out = Command::new(&self.config.ffmpeg)
                    .arg("-version")
                    .env_clear()
                    .stdin(Stdio::null())
                    .output()
                    .await
                    .map_err(|_| StageFailure {
                        code: FailureCode::EngineUnavailable,
                        message: "The audio encoder isn't available; it will be retried.".into(),
                    })?;
                Ok::<_, StageFailure>(
                    String::from_utf8_lossy(&out.stdout)
                        .lines()
                        .next()
                        .unwrap_or("ffmpeg")
                        .to_string(),
                )
            })
            .await?;
        Ok(format!(
            "encoder@{ENCODER_VERSION}/{version}/{LOUDNORM}/mp3-64k-cbr-mono-44100/id3v2.3"
        ))
    }

    /// Write the plan's segments and pauses to a WAV file, streaming segment by segment.
    async fn assemble(&self, plan: &SpeechPlan, out: &Path) -> Result<(), StageFailure> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: plan.sample_rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(out, spec)
            .map_err(|_| internal("Couldn't prepare the audio; it will be retried."))?;
        for planned in &plan.segments {
            let key = planned
                .key
                .parse()
                .map_err(|_| internal("The speech plan is corrupt."))?;
            let bytes = self
                .blobs
                .get(BlobKind::Segment, &key)
                .await
                .map_err(|_| internal("A speech segment is missing; it will be retried."))?;
            let pcm =
                Pcm::from_wav(&bytes).map_err(|_| internal("A speech segment is unreadable."))?;
            if pcm.sample_rate != plan.sample_rate {
                return Err(internal("A speech segment has the wrong sample rate."));
            }
            let silence = Pcm::silence(plan.sample_rate, planned.pause_after_ms);
            for s in pcm.samples.iter().chain(&silence) {
                writer
                    .write_sample(*s)
                    .map_err(|_| internal("Couldn't write the audio; it will be retried."))?;
            }
        }
        writer
            .finalize()
            .map_err(|_| internal("Couldn't write the audio; it will be retried."))
    }

    async fn encode(&self, item: &Item) -> Result<StageOutput, StageFailure> {
        let (Some(speech), Some(record)) = (&item.speech, &item.article) else {
            return Err(internal("There's no synthesised speech to encode."));
        };
        let plan: SpeechPlan = serde_json::from_slice(
            &self
                .blobs
                .get(BlobKind::Plan, &speech.plan_key)
                .await
                .map_err(|_| internal("The speech plan couldn't be read; it will be retried."))?,
        )
        .map_err(|_| internal("The speech plan is corrupt."))?;
        let article: Option<Article> = match self.blobs.get(BlobKind::Article, &record.key).await {
            Ok(bytes) => serde_json::from_slice(&bytes).ok(),
            Err(_) => None,
        };
        let byline = article
            .as_ref()
            .and_then(|a| a.byline.clone().or(a.site_name.clone()));
        let tags = Tags {
            title: tag(&record.title),
            artist: tag(byline.as_deref().unwrap_or("read-to-me")),
            album: "read-to-me".into(),
            comment: tag(&item.source_url),
        };
        let fingerprint = self.fingerprint().await?;
        let tags_json = serde_json::to_vec(&tags).map_err(|_| internal("Bad tags."))?;
        let key = episode_key(&speech.plan_key, &fingerprint, &tags_json);
        let path = self.blobs.local_path(BlobKind::Episode, &key);

        if path.is_file() {
            // Reused from an earlier attempt, which may have stopped before making it
            // durable.
            self.blobs
                .sync(BlobKind::Episode, &key)
                .await
                .map_err(|_| internal("The episode couldn't be stored; it will be retried."))?;
        } else {
            // Scratch files are removed when dropped: on failure, and also if this
            // future is dropped because the lease was lost or the server is stopping.
            let no_space =
                |_: std::io::Error| internal("No scratch space for encoding; it will be retried.");
            let wav = self.blobs.scratch("wav").map_err(no_space)?;
            let mp3 = self.blobs.scratch("mp3").map_err(no_space)?;
            self.encode_to(&plan, &tags, wav.path(), mp3.path()).await?;
            drop(wav);
            self.blobs
                .put_file(BlobKind::Episode, &key, mp3)
                .await
                .map_err(|_| internal("The episode couldn't be stored; it will be retried."))?;
        }
        let bytes = tokio::fs::metadata(&path)
            .await
            .map_err(|_| internal("The episode couldn't be stored; it will be retried."))?
            .len();
        Ok(StageOutput::Episode(EpisodeRecord {
            key,
            bytes,
            duration_ms: plan.duration_ms(),
        }))
    }

    async fn encode_to(
        &self,
        plan: &SpeechPlan,
        tags: &Tags,
        wav: &Path,
        mp3: &Path,
    ) -> Result<(), StageFailure> {
        self.assemble(plan, wav).await?;
        let input = wav.display().to_string();
        // Pass 1: measure loudness.
        let report = self
            .ffmpeg(&[
                "-i".into(),
                input.clone(),
                "-af".into(),
                format!("{LOUDNORM}:print_format=json"),
                "-f".into(),
                "null".into(),
                "-".into(),
            ])
            .await?;
        let m = measured(&report).ok_or_else(|| internal("Loudness analysis failed."))?;
        let field = |k: &str| m[k].as_str().unwrap_or("0").to_string();
        let filter = format!(
            "{LOUDNORM}:measured_I={}:measured_TP={}:measured_LRA={}:measured_thresh={}:offset={}:linear=true",
            field("input_i"),
            field("input_tp"),
            field("input_lra"),
            field("input_thresh"),
            field("target_offset"),
        );
        // Pass 2: normalise, resample and encode (ADR-0009).
        let mut args: Vec<String> = [
            "-y",
            "-i",
            &input,
            "-af",
            &filter,
            "-ar",
            "44100",
            "-ac",
            "1",
            "-c:a",
            "libmp3lame",
            "-b:a",
            "64k",
            "-id3v2_version",
            "3",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        for (name, value) in [
            ("title", &tags.title),
            ("artist", &tags.artist),
            ("album", &tags.album),
            ("comment", &tags.comment),
        ] {
            args.push("-metadata".into());
            args.push(format!("{name}={value}"));
        }
        args.extend([
            "-f".to_string(),
            "mp3".to_string(),
            mp3.display().to_string(),
        ]);
        self.ffmpeg(&args).await?;
        Ok(())
    }
}

impl StageRunner for EncodeStage {
    fn stage(&self) -> Stage {
        Stage::Encode
    }

    fn lease(&self) -> Duration {
        Duration::from_secs(300)
    }

    fn run<'a>(&'a self, item: &'a Item) -> StageFuture<'a> {
        Box::pin(self.encode(item))
    }
}

/// The publish stage: nothing to compute. Completing it publishes the item, which
/// stamps its publication time and puts it in the feed.
pub struct PublishStage;

impl StageRunner for PublishStage {
    fn stage(&self) -> Stage {
        Stage::Publish
    }

    fn lease(&self) -> Duration {
        Duration::from_secs(30)
    }

    fn run<'a>(&'a self, item: &'a Item) -> StageFuture<'a> {
        Box::pin(async move {
            if item.episode.is_none() {
                return Err(internal("There's no episode to publish."));
            }
            Ok(StageOutput::Nothing)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_cleaned_and_bounded() {
        assert_eq!(tag("  A\nTitle\u{0}  "), "ATitle");
        assert_eq!(tag(&"x".repeat(1000)).chars().count(), MAX_TAG_CHARS);
    }

    #[test]
    fn the_loudnorm_report_is_found() {
        let stderr = "noise\n[Parsed_loudnorm_0 @ 0x1]\n{\n\t\"input_i\" : \"-23.1\",\n\t\"target_offset\" : \"0.2\"\n}\n";
        let m = measured(stderr).unwrap();
        assert_eq!(m["input_i"], "-23.1");
        assert!(measured("no report").is_none());
    }
}
