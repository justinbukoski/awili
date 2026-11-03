//! Whisper model resolution + auto-download (slice-3 spec §1).
//!
//! Auto-downloads are restricted to a **pinned allowlist** of whisper.cpp models
//! (review slice-3 SF4): each known model id carries its exact filename, byte
//! size, and SHA-256 digest as published on the `ggerganov/whisper.cpp` Hugging
//! Face repo. A configured name outside the allowlist — including anything with
//! path separators or `..` — is rejected before any filesystem or network touch,
//! so the `[voice] model` value can never escape `Awili/models` nor fetch an
//! unpinned artifact. Downloads stream to a `.partial` sibling while a SHA-256 is
//! computed incrementally; only a byte-exact, digest-exact file is renamed into
//! place. A pre-existing final file is re-verified too, and quarantined (renamed
//! to `.invalid`) when corrupt so a truncated past download can't poison every
//! retry.
//!
//! An explicit `model_path` override skips all of this: the user pointed at a
//! file already on disk; nothing is downloaded and the file is trusted as theirs.
//!
//! The mapping/validation is pure and unit-tested; the download itself is
//! blocking network IO on the caller's worker thread (the same off-the-UI-thread
//! pattern providers use), reporting progress through a callback so the composer
//! can show a spinner on first use.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use crate::config::ModelSpec;

/// One allowlisted whisper.cpp model: the short config name, plus the pinned
/// size and SHA-256 of its `ggml-<name>.bin` on the whisper.cpp HF repo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KnownModel {
    pub name: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

/// The auto-download allowlist (review SF4). Sizes and SHA-256 digests are the
/// LFS metadata published by `huggingface.co/ggerganov/whisper.cpp` (fetched
/// 2026-06-11). Anything not listed here must be supplied via `model_path`.
pub const KNOWN_MODELS: &[KnownModel] = &[
    KnownModel {
        name: "tiny.en",
        size: 77_704_715,
        sha256: "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f",
    },
    KnownModel {
        name: "tiny.en-q5_1",
        size: 32_166_155,
        sha256: "c77c5766f1cef09b6b7d47f21b546cbddd4157886b3b5d6d4f709e91e66c7c2b",
    },
    KnownModel {
        name: "tiny",
        size: 77_691_713,
        sha256: "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21",
    },
    KnownModel {
        name: "base.en",
        size: 147_964_211,
        sha256: "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002",
    },
    KnownModel {
        name: "base.en-q5_1",
        size: 59_721_011,
        sha256: "4baf70dd0d7c4247ba2b81fafd9c01005ac77c2f9ef064e00dcf195d0e2fdd2f",
    },
    KnownModel {
        name: "base",
        size: 147_951_465,
        sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
    },
    KnownModel {
        name: "base-q5_1",
        size: 59_707_625,
        sha256: "422f1ae452ade6f30a004d7e5c6a43195e4433bc370bf23fac9cc591f01a8898",
    },
    KnownModel {
        name: "small.en",
        size: 487_614_201,
        sha256: "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d",
    },
    KnownModel {
        name: "small.en-q5_1",
        size: 190_098_681,
        sha256: "bfdff4894dcb76bbf647d56263ea2a96645423f1669176f4844a1bf8e478ad30",
    },
    KnownModel {
        name: "small",
        size: 487_601_967,
        sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b",
    },
    KnownModel {
        name: "small-q5_1",
        size: 190_085_487,
        sha256: "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
    },
];

/// Look a configured model name up in the allowlist. Rejects, in order: empty or
/// over-long names, anything containing a path separator / `..` / a leading dot
/// (defence in depth — the allowlist itself contains none of these), then
/// anything simply not pinned. Pure; called before any filesystem or network IO.
pub fn validate_model_name(name: &str) -> Result<&'static KnownModel, ModelError> {
    // Length first: a hostile config can't make us scan/format megabytes.
    if name.is_empty() || name.len() > 64 {
        return Err(ModelError::InvalidName(format!(
            "model name must be 1..=64 chars (got {})",
            name.len()
        )));
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") || name.starts_with('.') {
        return Err(ModelError::InvalidName(format!(
            "model name {name:?} contains path components; set voice.model_path for a local file"
        )));
    }
    KNOWN_MODELS.iter().find(|m| m.name == name).ok_or_else(|| {
        let known: Vec<&str> = KNOWN_MODELS.iter().map(|m| m.name).collect();
        ModelError::InvalidName(format!(
            "unknown model {name:?}; auto-download is limited to {known:?} — for anything else, \
             download it yourself and set voice.model_path"
        ))
    })
}

/// Where downloaded whisper models live: `<data_dir>/Awili/models`. Mirrors the
/// `Awili` data-dir folder `awili-persist` uses for the database.
pub fn models_dir() -> Option<PathBuf> {
    // Windows: models are large and per-machine, so they live in
    // `%LOCALAPPDATA%\Awili\models`, not the roaming profile. Other
    // platforms are unchanged.
    #[cfg(windows)]
    let base = dirs::data_local_dir();
    #[cfg(not(windows))]
    let base = dirs::data_dir();
    base.map(|d| d.join(awili_config::APP_DIR_NAME).join("models"))
}

/// The on-disk filename for a model name, e.g. `base.en-q5_1` → `ggml-base.en-q5_1.bin`.
pub fn model_filename(name: &str) -> String {
    format!("ggml-{name}.bin")
}

/// The whisper.cpp Hugging Face download URL for a model name. Quantized and
/// full models alike live under `ggerganov/whisper.cpp` as `ggml-*.bin`.
pub fn model_url(name: &str) -> String {
    format!(
        "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}",
        model_filename(name)
    )
}

/// Resolve a [`ModelSpec`] to the path the model should live at. An explicit
/// `path_override` wins verbatim; otherwise it is the download target in
/// [`models_dir`]. Returns `None` only when the platform exposes no data dir and
/// no override was given.
pub fn resolve_path(spec: &ModelSpec) -> Option<PathBuf> {
    if let Some(p) = &spec.path_override {
        return Some(p.clone());
    }
    models_dir().map(|d| d.join(model_filename(&spec.name)))
}

/// Progress of an in-flight download: bytes received so far and the total when
/// the server reported a `Content-Length` (`None` for a chunked response).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub received: u64,
    pub total: Option<u64>,
}

impl Progress {
    /// Completion fraction in `0.0..=1.0`, or `None` when the total is unknown.
    pub fn fraction(&self) -> Option<f32> {
        self.total
            .filter(|t| *t > 0)
            .map(|t| (self.received as f32 / t as f32).min(1.0))
    }
}

/// Why a model could not be made available.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("no data dir for model storage; set voice.model_path")]
    NoDataDir,
    #[error("{0}")]
    InvalidName(String),
    #[error("model download failed: {0}")]
    Download(String),
    #[error("model verification failed: {0}")]
    Verify(String),
    #[error("model file io: {0}")]
    Io(#[from] std::io::Error),
}

/// SHA-256 of a file as a lowercase hex string. Streams in 64 KiB chunks so a
/// half-gigabyte model never loads into memory at once.
fn sha256_file(path: &Path) -> Result<String, std::io::Error> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Check an on-disk file against a pinned model: exact size, then exact SHA-256.
/// Returns the failure reason, or `None` when the file is genuine.
pub fn verify_file(path: &Path, model: &KnownModel) -> Result<(), ModelError> {
    let len = fs::metadata(path)?.len();
    if len != model.size {
        return Err(ModelError::Verify(format!(
            "{}: size {len} != pinned {} bytes",
            path.display(),
            model.size
        )));
    }
    let digest = sha256_file(path)?;
    if digest != model.sha256 {
        return Err(ModelError::Verify(format!(
            "{}: sha256 {digest} != pinned {}",
            path.display(),
            model.sha256
        )));
    }
    Ok(())
}

/// Ensure the model for `spec` is present on disk, downloading it (with progress)
/// only when missing, and return its path. Blocking: call on a worker thread.
///
/// With a `path_override`, the user's file is returned as-is — no allowlist, no
/// download, no checksum (it never came from us). By name, the model must be
/// allowlisted ([`validate_model_name`]); an existing file is re-verified against
/// the pinned size+SHA-256 (a corrupt one is quarantined to `.invalid` and
/// re-downloaded); a fresh download is verified for completed length against both
/// `Content-Length` and the pinned size, plus the pinned digest, *before* the
/// atomic rename into place (review SF4).
pub fn ensure_model(
    spec: &ModelSpec,
    mut on_progress: impl FnMut(Progress),
) -> Result<PathBuf, ModelError> {
    if let Some(p) = &spec.path_override {
        return Ok(p.clone());
    }
    let known = validate_model_name(&spec.name)?;
    let path = models_dir()
        .ok_or(ModelError::NoDataDir)?
        .join(model_filename(known.name));

    if path.exists() {
        match verify_file(&path, known) {
            Ok(()) => return Ok(path),
            Err(e) => {
                // A truncated/corrupt past download must not be trusted forever
                // (review SF4): quarantine it and fall through to a fresh fetch.
                eprintln!("awili: cached voice model failed verification ({e}); re-downloading");
                let quarantine = path.with_extension("invalid");
                fs::rename(&path, &quarantine)?;
            }
        }
    }
    if let Some(parent) = path.parent() {
        awili_config::ensure_private_app_dir(parent)?;
    }
    let url = model_url(known.name);
    download(
        &url,
        &path,
        known,
        DownloadLimits::for_size(known.size),
        &mut on_progress,
    )?;
    Ok(path)
}

/// How long a model download may go without receiving a single byte before it
/// is given up as stalled (review F0954). Blocking reqwest applies its `timeout`
/// to the request and to each body read separately, so this is a stall limit,
/// not a cap on the whole transfer.
const DOWNLOAD_STALL_TIMEOUT: Duration = Duration::from_secs(60);
/// The slowest average speed a model download may run at: the overall deadline
/// is the pinned size at this speed (about two hours for the largest model).
const DOWNLOAD_MIN_BYTES_PER_SEC: u64 = 64 * 1024;
/// The overall deadline is never shorter than this, so a small model on a slow
/// line is not cut off.
const DOWNLOAD_MIN_DEADLINE: Duration = Duration::from_secs(10 * 60);

/// The time limits for one model download (review F0954): a stall limit on every
/// wait for the server, and an overall deadline for the whole transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DownloadLimits {
    stall: Duration,
    overall: Duration,
}

impl DownloadLimits {
    /// The limits for a model of `size` bytes.
    fn for_size(size: u64) -> Self {
        let at_min_speed = Duration::from_secs(size / DOWNLOAD_MIN_BYTES_PER_SEC);
        Self {
            stall: DOWNLOAD_STALL_TIMEOUT,
            overall: at_min_speed.max(DOWNLOAD_MIN_DEADLINE),
        }
    }
}

/// Whether a body read failed on the stall limit. Blocking reqwest reports it
/// as an `Other` I/O error wrapping a timeout `reqwest::Error`.
fn read_timed_out(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::TimedOut
        || e.get_ref()
            .and_then(|inner| inner.downcast_ref::<reqwest::Error>())
            .is_some_and(reqwest::Error::is_timeout)
}

/// Plain words for a duration in an error message: whole minutes, or seconds
/// below one minute.
fn plain_duration(d: Duration) -> String {
    let secs = d.as_secs().max(1);
    let (n, unit) = if secs >= 60 {
        (secs / 60, "minute")
    } else {
        (secs, "second")
    };
    if n == 1 {
        format!("1 {unit}")
    } else {
        format!("{n} {unit}s")
    }
}

/// Stream `url` to `dest` via a `.partial` temp file, hashing as bytes arrive,
/// and rename into place only after length + digest verification. Gives up when
/// the server sends nothing for `limits.stall` or the whole download outlasts
/// `limits.overall`, removing the `.partial` file (review F0954).
fn download(
    url: &str,
    dest: &Path,
    known: &KnownModel,
    limits: DownloadLimits,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<(), ModelError> {
    let started = Instant::now();
    let client = reqwest::blocking::Client::builder()
        .timeout(limits.stall)
        .connect_timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| ModelError::Download(e.to_string()))?;
    let stalled = || {
        ModelError::Download(format!(
            "the server sent nothing for {}; try again later",
            plain_duration(limits.stall)
        ))
    };
    let mut resp = client.get(url).send().map_err(|e| {
        if e.is_timeout() {
            stalled()
        } else {
            ModelError::Download(e.to_string())
        }
    })?;
    if !resp.status().is_success() {
        return Err(ModelError::Download(format!(
            "HTTP {} for {url}",
            resp.status()
        )));
    }
    let total = resp.content_length();
    // A server advertising a length other than the pinned size can't be right —
    // fail before transferring hundreds of MB.
    if let Some(t) = total {
        if t != known.size {
            return Err(ModelError::Verify(format!(
                "server advertises {t} bytes for {} but {} bytes are pinned",
                known.name, known.size
            )));
        }
    }

    let partial = dest.with_extension("partial");
    let mut file = fs::File::create(&partial)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut received = 0u64;
    on_progress(Progress { received: 0, total });
    let transfer = (|| {
        loop {
            if started.elapsed() > limits.overall {
                return Err(ModelError::Download(format!(
                    "still unfinished after {}; try again later",
                    plain_duration(limits.overall)
                )));
            }
            let n = resp.read(&mut buf).map_err(|e| {
                if read_timed_out(&e) {
                    stalled()
                } else {
                    ModelError::Download(e.to_string())
                }
            })?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n])?;
            hasher.update(&buf[..n]);
            received += n as u64;
            on_progress(Progress { received, total });
        }
        file.flush()?;
        Ok(())
    })();
    drop(file);
    if let Err(e) = transfer {
        let _ = fs::remove_file(&partial);
        return Err(e);
    }

    // Completed-length check: against Content-Length when given, and always
    // against the pinned size — EOF on a dropped connection is not success.
    let check = (|| {
        if let Some(t) = total {
            if received != t {
                return Err(ModelError::Verify(format!(
                    "connection ended at {received} of {t} bytes"
                )));
            }
        }
        if received != known.size {
            return Err(ModelError::Verify(format!(
                "received {received} bytes, pinned size is {}",
                known.size
            )));
        }
        let digest = hex(&hasher.finalize());
        if digest != known.sha256 {
            return Err(ModelError::Verify(format!(
                "sha256 {digest} does not match pinned {}",
                known.sha256
            )));
        }
        Ok(())
    })();
    if let Err(e) = check {
        let _ = fs::remove_file(&partial);
        return Err(e);
    }

    fs::rename(&partial, dest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("awili-voice-test-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn filename_and_url_follow_whisper_cpp_naming() {
        assert_eq!(model_filename("base.en"), "ggml-base.en.bin");
        assert_eq!(model_filename("base.en-q5_1"), "ggml-base.en-q5_1.bin");
        assert_eq!(
            model_url("small.en"),
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.en.bin"
        );
    }

    #[test]
    fn explicit_path_override_wins_over_download_dir() {
        let spec = ModelSpec {
            name: "base.en".to_string(),
            path_override: Some(PathBuf::from("/custom/model.bin")),
        };
        assert_eq!(
            resolve_path(&spec),
            Some(PathBuf::from("/custom/model.bin"))
        );
    }

    #[test]
    fn name_resolves_under_models_dir() {
        let spec = ModelSpec {
            name: "base.en-q5_1".to_string(),
            path_override: None,
        };
        // Only assert the shape when the platform has a data dir (CI always does).
        if let Some(p) = resolve_path(&spec) {
            assert!(
                p.ends_with(format!(
                    "{}/models/ggml-base.en-q5_1.bin",
                    awili_config::APP_DIR_NAME
                )),
                "got {p:?}"
            );
        }
    }

    #[test]
    fn progress_fraction_is_clamped_and_optional() {
        assert_eq!(
            Progress {
                received: 50,
                total: Some(100)
            }
            .fraction(),
            Some(0.5)
        );
        assert_eq!(
            Progress {
                received: 200,
                total: Some(100)
            }
            .fraction(),
            Some(1.0)
        );
        assert_eq!(
            Progress {
                received: 5,
                total: None
            }
            .fraction(),
            None
        );
        assert_eq!(
            Progress {
                received: 5,
                total: Some(0)
            }
            .fraction(),
            None
        );
    }

    #[test]
    fn ensure_returns_existing_file_without_download() {
        // A pre-existing override path is returned as-is, proving the early-out
        // before any network touch — and that overrides skip the allowlist (the
        // user's own file is trusted as theirs).
        let dir = temp_dir("override");
        let model = dir.join("ggml-custom-finetune.bin");
        fs::write(&model, b"not really a model").unwrap();
        let spec = ModelSpec {
            name: "anything-at-all".to_string(),
            path_override: Some(model.clone()),
        };
        let got = ensure_model(&spec, |_| panic!("must not download when overridden")).unwrap();
        assert_eq!(got, model);
        fs::remove_dir_all(&dir).ok();
    }

    // ---- SF4: allowlist + traversal rejection --------------------------------

    #[test]
    fn allowlisted_names_validate_and_default_is_pinned() {
        let m = validate_model_name("base.en-q5_1").unwrap();
        assert_eq!(m.size, 59_721_011);
        assert_eq!(
            m.sha256,
            "4baf70dd0d7c4247ba2b81fafd9c01005ac77c2f9ef064e00dcf195d0e2fdd2f"
        );
        // The config default must always be downloadable.
        let default = crate::config::VoiceConfig::default().model;
        assert!(
            validate_model_name(&default).is_ok(),
            "default model {default:?} not pinned"
        );
    }

    #[test]
    fn path_traversal_names_are_rejected_before_any_io() {
        // The SF4 regression: a [voice] model value must never escape Awili/models
        // nor reach the network. Every shape of separator/parent-dir smuggling is
        // refused by validation alone — ensure_model errors with InvalidName and
        // no file or socket is touched.
        for evil in [
            "../base.en",
            "..",
            "models/../../etc/cron.d/x",
            "a/b",
            "a\\b",
            ".hidden",
            "base.en/..",
            "",
            // over-long: length is validated before anything else
            &"x".repeat(65),
        ] {
            let err = validate_model_name(evil).unwrap_err();
            assert!(
                matches!(err, ModelError::InvalidName(_)),
                "{evil:?} -> {err}"
            );
            let spec = ModelSpec {
                name: evil.to_string(),
                path_override: None,
            };
            let err =
                ensure_model(&spec, |_| panic!("no progress for a rejected name")).unwrap_err();
            assert!(
                matches!(err, ModelError::InvalidName(_)),
                "{evil:?} -> {err}"
            );
        }
    }

    #[test]
    fn unpinned_names_are_rejected_with_the_allowlist_in_the_message() {
        let err = validate_model_name("large-v3").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("base.en"),
            "message should list known models: {msg}"
        );
        assert!(
            msg.contains("model_path"),
            "message should point at the override: {msg}"
        );
    }

    #[test]
    fn no_allowlisted_name_contains_path_components() {
        // The defence-in-depth invariant the separator check backs up.
        for m in KNOWN_MODELS {
            assert!(validate_model_name(m.name).is_ok());
            assert!(!m.name.contains('/') && !m.name.contains('\\') && !m.name.contains(".."));
            assert_eq!(m.sha256.len(), 64);
            assert!(m.sha256.chars().all(|c| c.is_ascii_hexdigit()));
            assert!(m.size > 0);
        }
    }

    // ---- SF4: size + digest verification --------------------------------------

    #[test]
    fn verify_file_checks_size_then_digest() {
        let dir = temp_dir("verify");
        let path = dir.join("ggml-fake.bin");
        fs::write(&path, b"hello").unwrap();

        // sha256("hello")
        let good = KnownModel {
            name: "fake",
            size: 5,
            sha256: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        };
        assert!(verify_file(&path, &good).is_ok());

        let wrong_size = KnownModel { size: 6, ..good };
        let e = verify_file(&path, &wrong_size).unwrap_err();
        assert!(matches!(e, ModelError::Verify(_)), "{e}");

        let wrong_hash = KnownModel {
            sha256: "0000000000000000000000000000000000000000000000000000000000000000",
            ..good
        };
        let e = verify_file(&path, &wrong_hash).unwrap_err();
        assert!(matches!(e, ModelError::Verify(_)), "{e}");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sha256_streaming_matches_known_vector() {
        let dir = temp_dir("sha");
        let path = dir.join("empty.bin");
        fs::write(&path, b"").unwrap();
        // sha256 of the empty string.
        assert_eq!(
            sha256_file(&path).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        fs::remove_dir_all(&dir).ok();
    }

    // --- F0954: a download that stalls or crawls is given up, not waited on ---

    /// A pretend model for the local test server: 1000 bytes, any digest.
    const SLOW_MODEL: KnownModel = KnownModel {
        name: "slow-test",
        size: 1000,
        sha256: "0000000000000000000000000000000000000000000000000000000000000000",
    };

    /// Serve one request on a local port: send the headers for a 1000-byte body,
    /// then `bytes` body bytes `gap` apart, then hold the connection open until
    /// the client goes away. Returns the URL.
    fn slow_server(bytes: usize, gap: Duration) -> String {
        use std::io::BufRead;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/model.bin", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).is_ok_and(|n| n > 0) && line != "\r\n" {
                line.clear();
            }
            let mut out = stream;
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                SLOW_MODEL.size
            );
            if out.write_all(head.as_bytes()).is_err() {
                return;
            }
            for _ in 0..bytes {
                std::thread::sleep(gap);
                if out.write_all(b"x").and_then(|()| out.flush()).is_err() {
                    return;
                }
            }
            // Hold the connection open; the client's limits must end the wait.
            std::thread::sleep(Duration::from_secs(30));
        });
        url
    }

    /// Run `download` on a worker and wait at most `wait` for it to give up.
    fn download_within(
        url: String,
        dest: PathBuf,
        limits: DownloadLimits,
        wait: Duration,
    ) -> Result<Result<(), ModelError>, std::sync::mpsc::RecvTimeoutError> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = download(&url, &dest, &SLOW_MODEL, limits, &mut |_| {});
            let _ = tx.send(result);
        });
        rx.recv_timeout(wait)
    }

    #[test]
    fn rc2_voice_a_stalled_model_download_gives_up() {
        let dir = temp_dir("stall");
        let dest = dir.join("ggml-slow-test.bin");
        let url = slow_server(10, Duration::from_millis(10));
        let limits = DownloadLimits {
            stall: Duration::from_millis(500),
            overall: Duration::from_secs(600),
        };
        let outcome = download_within(url, dest.clone(), limits, Duration::from_secs(10))
            .expect("a stalled download must give up on its own");
        let err = outcome.expect_err("a stalled download is a failure");
        assert!(
            err.to_string()
                .contains("the server sent nothing for 1 second;"),
            "plain stall message, got: {err}"
        );
        assert!(!dest.with_extension("partial").exists(), "partial removed");
        assert!(!dest.exists(), "nothing renamed into place");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rc2_voice_a_crawling_model_download_hits_its_overall_deadline() {
        let dir = temp_dir("crawl");
        let dest = dir.join("ggml-slow-test.bin");
        // A byte every 50 ms never trips the stall limit, but never finishes.
        let url = slow_server(1000, Duration::from_millis(50));
        let limits = DownloadLimits {
            stall: Duration::from_secs(5),
            overall: Duration::from_millis(600),
        };
        let outcome = download_within(url, dest.clone(), limits, Duration::from_secs(10))
            .expect("a crawling download must give up at its deadline");
        let err = outcome.expect_err("a download past its deadline is a failure");
        assert!(
            err.to_string().contains("still unfinished after"),
            "plain deadline message, got: {err}"
        );
        assert!(!dest.with_extension("partial").exists(), "partial removed");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rc2_voice_download_limits_scale_with_the_model_size() {
        let small = DownloadLimits::for_size(32_166_155);
        assert_eq!(small.stall, DOWNLOAD_STALL_TIMEOUT);
        assert_eq!(
            small.overall, DOWNLOAD_MIN_DEADLINE,
            "floor for small models"
        );
        let large = DownloadLimits::for_size(487_614_201);
        assert_eq!(
            large.overall,
            Duration::from_secs(487_614_201 / (64 * 1024))
        );
        assert!(
            large.overall > Duration::from_secs(60 * 60),
            "about two hours"
        );
        assert_eq!(plain_duration(Duration::from_secs(600)), "10 minutes");
        assert_eq!(plain_duration(Duration::from_millis(500)), "1 second");
        assert_eq!(plain_duration(Duration::from_secs(60)), "1 minute");
    }
}
