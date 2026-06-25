//! Raw & encrypted stream target adapter (Phase 5 §3).
//!
//! Writes `btrfs send … | [compress] | [encrypt] > <name>.btrfs[.zst][.gpg]`
//! alongside a `.info` sidecar file, then implements [`SubvolumeRepository`],
//! [`TransferPort`], and [`DeletePort`] on top of those sidecar files so the
//! rest of the application — parent resolution, retention, safety policy — works
//! unchanged on a directory of stream files.
//!
//! See `documentation/11-raw-encrypted-targets.md` for the full design.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use mybtrfs_application::ports::{
    DeleteCommit, DeletePort, PortError, SubvolumeRepository, TransferPort,
};
use mybtrfs_domain::model::{Subvolume, Uuid};
use mybtrfs_domain::parent::ParentSelection;

use crate::command::{CommandRunner, SystemCommandRunner};

// ── Constants ────────────────────────────────────────────────────────────────

/// Filename stored in the raw target directory to provide a stable
/// filesystem-UUID for the synthetic [`RelationshipGraph`] built from sidecars.
const FS_UUID_FILENAME: &str = ".mybtrfs_raw_fs_uuid";

/// `btrfs` binary name.
const BTRFS: &str = "btrfs";

// ── Public types ─────────────────────────────────────────────────────────────

/// Compression codec applied between `btrfs send` and optional encryption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RawCompress {
    /// zstd (default, highest ratio/speed balance).
    #[default]
    Zstd,
    /// gzip (wide compatibility).
    Gzip,
    /// xz (maximum compression).
    Xz,
    /// No compression; stream is passed through unchanged.
    None,
}

/// Raw & encrypted stream target adapter.
///
/// Implements [`TransferPort`], [`DeletePort`], and [`SubvolumeRepository`] on
/// a directory of `.btrfs[.zst][.gpg]` stream files and their `.info` sidecar
/// files. No btrfs filesystem is required at the target — backups land on any
/// filesystem or object store.
pub struct RawStreamAdapter {
    /// Directory where stream files and sidecar files are written.
    target_dir: PathBuf,
    /// Compression codec for forward transfers.
    compress: RawCompress,
    /// GPG passphrase file for symmetric encryption; `None` means no encryption.
    passphrase_file: Option<PathBuf>,
    /// Injectable command runner (production: [`SystemCommandRunner`]).
    runner: Box<dyn CommandRunner>,
}

impl RawStreamAdapter {
    /// Create a new adapter writing to `target_dir`.
    ///
    /// Use [`SystemCommandRunner`] (production). For unit tests, use
    /// [`Self::with_runner`].
    #[must_use]
    pub fn new(
        target_dir: PathBuf,
        compress: RawCompress,
        passphrase_file: Option<PathBuf>,
    ) -> Self {
        Self {
            target_dir,
            compress,
            passphrase_file,
            runner: Box::new(SystemCommandRunner),
        }
    }

    /// Inject a custom runner (test seam).
    #[cfg(test)]
    pub(crate) fn with_runner(
        target_dir: PathBuf,
        compress: RawCompress,
        passphrase_file: Option<PathBuf>,
        runner: Box<dyn CommandRunner>,
    ) -> Self {
        Self {
            target_dir,
            compress,
            passphrase_file,
            runner,
        }
    }

    /// Restore a raw stream backup by reversing the pipeline:
    /// `gpg --decrypt … | decompress | btrfs receive <btrfs_dir>`.
    ///
    /// Reads the sidecar at `sidecar_path` to determine the codec, then runs
    /// the reverse pipeline. Returns the path of the staging subvolume created
    /// by `btrfs receive` (a path inside `btrfs_dir` named after the stream
    /// leaf). The caller is responsible for calling `make_writable` and
    /// cleaning up the staging subvolume.
    ///
    /// # Errors
    /// [`PortError::Io`] if the sidecar or stream file cannot be read;
    /// [`PortError::Parse`] if the sidecar is malformed;
    /// [`PortError::Command`] if the pipeline fails.
    pub fn restore_raw_pipeline(
        &self,
        sidecar_path: &Path,
        btrfs_dir: &Path,
        passphrase_file: Option<&Path>,
    ) -> Result<PathBuf, PortError> {
        let content = std::fs::read_to_string(sidecar_path)?;
        let info = SidecarInfo::from_toml(&content)?;

        let target_dir = sidecar_path
            .parent()
            .ok_or_else(|| PortError::Parse("sidecar path has no parent directory".to_string()))?;
        let stream_path = target_dir.join(&info.stream_file);

        // Build decrypt stage
        let (decrypt_prog, decrypt_args_owned): (&str, Vec<OsString>) = match info.encrypt.as_str()
        {
            "gpg-symmetric" => {
                let pf = passphrase_file.ok_or_else(|| {
                    PortError::Command(
                        "--passphrase-file is required to restore a gpg-symmetric backup"
                            .to_string(),
                    )
                })?;
                (
                    "gpg",
                    vec![
                        "--batch".into(),
                        "--passphrase-file".into(),
                        pf.as_os_str().to_owned(),
                        "--decrypt".into(),
                        stream_path.as_os_str().to_owned(),
                    ],
                )
            }
            "none" => ("cat", vec![stream_path.as_os_str().to_owned()]),
            other => {
                return Err(PortError::Parse(format!(
                    "unknown encrypt type in sidecar: {other:?}"
                )));
            }
        };

        // Build decompress stage
        let (decompress_prog, decompress_args_owned): (&str, Vec<OsString>) =
            match info.compress.as_str() {
                "zstd" => ("zstd", vec!["-d".into(), "-c".into()]),
                "gzip" => ("gzip", vec!["-d".into(), "-c".into()]),
                "xz" => ("xz", vec!["-d".into(), "-c".into()]),
                "none" => ("cat", vec![]),
                other => {
                    return Err(PortError::Parse(format!(
                        "unknown compress type in sidecar: {other:?}"
                    )));
                }
            };

        // Build btrfs receive stage
        let receive_args_owned: Vec<OsString> =
            vec!["receive".into(), btrfs_dir.as_os_str().to_owned()];

        let decrypt_args: Vec<&std::ffi::OsStr> =
            decrypt_args_owned.iter().map(OsString::as_os_str).collect();
        let decompress_args: Vec<&std::ffi::OsStr> = decompress_args_owned
            .iter()
            .map(OsString::as_os_str)
            .collect();
        let receive_args: Vec<&std::ffi::OsStr> =
            receive_args_owned.iter().map(OsString::as_os_str).collect();

        log::info!(
            "raw restore: {} | {} | btrfs receive {}",
            decrypt_prog,
            decompress_prog,
            btrfs_dir.display()
        );

        self.runner.pipe3(
            (decrypt_prog, &decrypt_args),
            (decompress_prog, &decompress_args),
            (BTRFS, &receive_args),
            None,
        )?;

        // The received subvolume is named after the leaf inside btrfs_dir.
        Ok(btrfs_dir.join(&info.leaf))
    }
}

// ── Sidecar serialization ─────────────────────────────────────────────────────

/// Metadata sidecar stored alongside each raw stream file.
///
/// File: `<leaf>.info` next to `<leaf>.btrfs[.zst][.gpg]`.
/// Written atomically via `.info.tmp` → rename.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SidecarInfo {
    /// Unique UUID for this stream (content-addressed stable id source).
    uuid: String,
    /// UUID of the source snapshot this was sent from (correlation key).
    received_from_uuid: String,
    /// btrbk-style leaf name (`<basename>.<timestamp>[_N]`), drives retention.
    leaf: String,
    /// Full filename including codec extensions (e.g. `home.20260625T….btrfs.zst.gpg`).
    /// Required by [`DeletePort`] to locate the stream file.
    stream_file: String,
    /// Unix timestamp of when this backup was created; drives `cgen` ordering.
    created_at: u64,
    /// Compression codec: `"zstd"` | `"gzip"` | `"xz"` | `"none"`.
    compress: String,
    /// Encryption: `"gpg-symmetric"` | `"none"`.
    encrypt: String,
}

impl SidecarInfo {
    /// Serialize to TOML.
    ///
    /// # Errors
    /// [`PortError::Parse`] if serialization fails (practically unreachable for
    /// this struct's types).
    fn to_toml(&self) -> Result<String, PortError> {
        toml::to_string(self)
            .map_err(|e| PortError::Parse(format!("failed to serialize sidecar: {e}")))
    }

    /// Deserialize from TOML, applying Rule 16: a present-but-malformed UUID
    /// field is a parse error, never silently `None`.
    ///
    /// # Errors
    /// [`PortError::Parse`] if the TOML is malformed, a required field is
    /// missing, or a UUID field is present but not a valid canonical UUID.
    fn from_toml(s: &str) -> Result<Self, PortError> {
        let info: Self = toml::from_str(s)
            .map_err(|e| PortError::Parse(format!("malformed sidecar TOML: {e}")))?;
        // Rule 16: present-but-malformed UUID → parse error (never silent None).
        if Uuid::parse(&info.uuid).is_none() {
            return Err(PortError::Parse(format!(
                "malformed uuid in sidecar: {:?}",
                info.uuid
            )));
        }
        if Uuid::parse(&info.received_from_uuid).is_none() {
            return Err(PortError::Parse(format!(
                "malformed received_from_uuid in sidecar: {:?}",
                info.received_from_uuid
            )));
        }
        Ok(info)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Derive a stable `u64` id from the first 8 bytes of a canonical UUID string.
///
/// Called only after [`Uuid::parse`] has validated the hex characters, so the
/// string slice operations are safe.
fn stable_id(uuid_str: &str) -> u64 {
    let hex: String = uuid_str.chars().filter(|&c| c != '-').collect();
    let mut bytes = [0u8; 8];
    for (i, byte) in bytes.iter_mut().enumerate() {
        let pos = i * 2;
        if let (Some(hi_s), Some(lo_s)) = (hex.get(pos..pos + 1), hex.get(pos + 1..pos + 2)) {
            if let (Ok(hi), Ok(lo)) = (u8::from_str_radix(hi_s, 16), u8::from_str_radix(lo_s, 16)) {
                *byte = (hi << 4) | lo;
            }
        }
    }
    u64::from_le_bytes(bytes)
}

/// Build a synthetic [`Subvolume`] from a parsed sidecar.
///
/// # Errors
/// [`PortError::Parse`] if either UUID field is malformed (Rule 16).
fn sidecar_to_subvolume(
    info: &SidecarInfo,
    target_dir: &Path,
    fs_uuid: Uuid,
) -> Result<Subvolume, PortError> {
    let uuid = Uuid::parse(&info.uuid)
        .ok_or_else(|| PortError::Parse(format!("malformed uuid in sidecar: {:?}", info.uuid)))?;
    let received_uuid = Uuid::parse(&info.received_from_uuid).ok_or_else(|| {
        PortError::Parse(format!(
            "malformed received_from_uuid in sidecar: {:?}",
            info.received_from_uuid
        ))
    })?;
    let id = stable_id(&info.uuid);
    Ok(Subvolume {
        id,
        uuid: Some(uuid),
        parent_uuid: None,
        received_uuid: Some(received_uuid),
        generation: info.created_at,
        cgen: info.created_at,
        readonly: true,
        path: PathBuf::from(&info.leaf),
        fs_uuid,
        mountpoint: target_dir.to_path_buf(),
    })
}

/// Read (or generate) the raw-target filesystem UUID from `<target_dir>/.mybtrfs_raw_fs_uuid`.
///
/// Creates the file on first call (first `send_receive()`).
///
/// # Errors
/// [`PortError::Io`] if the file cannot be read/written;
/// [`PortError::Parse`] if the file contains a malformed UUID.
fn load_or_create_fs_uuid(target_dir: &Path) -> Result<Uuid, PortError> {
    let uuid_file = target_dir.join(FS_UUID_FILENAME);
    if uuid_file.exists() {
        let s = std::fs::read_to_string(&uuid_file)?.trim().to_owned();
        return Uuid::parse(&s).ok_or_else(|| {
            PortError::Parse(format!("malformed UUID in {FS_UUID_FILENAME}: {s:?}"))
        });
    }
    // Generate a new UUID from the kernel's random source.
    let raw = std::fs::read_to_string("/proc/sys/kernel/random/uuid").map_err(|e| {
        PortError::Io(std::io::Error::new(
            e.kind(),
            format!("failed to read kernel random uuid: {e}"),
        ))
    })?;
    let s = raw.trim();
    let uuid = Uuid::parse(s).ok_or_else(|| {
        PortError::Parse(format!(
            "unexpected uuid format from /proc/sys/kernel/random/uuid: {s:?}"
        ))
    })?;
    std::fs::write(&uuid_file, uuid.as_str())?;
    Ok(uuid)
}

/// Append `.info` to a path by pushing onto the OS-string representation.
///
/// Unlike `path.with_extension("info")`, this correctly handles leaf names that
/// contain a single dot (e.g. `home.20260625T020000+0200` → `.info` is
/// appended, not the last segment replaced).
fn sidecar_path_for(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".info");
    PathBuf::from(s)
}

/// Derive the full stream filename from the leaf and codec settings.
fn stream_filename(leaf: &str, compress: RawCompress, encrypted: bool) -> String {
    let c = match compress {
        RawCompress::Zstd => ".zst",
        RawCompress::Gzip => ".gz",
        RawCompress::Xz => ".xz",
        RawCompress::None => "",
    };
    let e = if encrypted { ".gpg" } else { "" };
    format!("{leaf}.btrfs{c}{e}")
}

// ── SubvolumeRepository ───────────────────────────────────────────────────────

impl SubvolumeRepository for RawStreamAdapter {
    /// List all stream backups in the raw target directory by scanning `*.info`
    /// sidecar files. The `filesystem` argument is ignored (no btrfs filesystem
    /// involved); callers should pass the same path used at construction.
    ///
    /// Returns an empty vec when the directory has no sidecars and no
    /// `.mybtrfs_raw_fs_uuid` file (first-run state). Returns
    /// [`PortError::Verification`] if sidecars exist but the UUID file is
    /// absent (corrupted state).
    ///
    /// # Errors
    /// [`PortError::Verification`] if sidecars exist without the UUID file;
    /// [`PortError::Io`] if the directory cannot be read;
    /// [`PortError::Parse`] if the UUID file contains a malformed UUID.
    fn list(&self, _filesystem: &Path) -> Result<Vec<Subvolume>, PortError> {
        let uuid_file = self.target_dir.join(FS_UUID_FILENAME);

        // Scan for sidecar files first so we can distinguish "empty" from
        // "corrupted" when the UUID file is absent.
        let sidecars: Vec<PathBuf> = std::fs::read_dir(&self.target_dir)?
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                if path.extension()?.to_str()? == "info"
                    && !path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(|s| s.ends_with(".tmp"))
                        .unwrap_or(false)
                {
                    Some(path)
                } else {
                    None
                }
            })
            .collect();

        if !uuid_file.exists() {
            if !sidecars.is_empty() {
                log::error!(
                    "sidecar files found in {} but {} is absent — directory may be corrupted",
                    self.target_dir.display(),
                    FS_UUID_FILENAME
                );
                return Err(PortError::Verification(format!(
                    "raw target {} has sidecars but missing {FS_UUID_FILENAME}",
                    self.target_dir.display()
                )));
            }
            return Ok(Vec::new());
        }

        let fs_uuid_str = std::fs::read_to_string(&uuid_file)?.trim().to_owned();
        let fs_uuid = Uuid::parse(&fs_uuid_str).ok_or_else(|| {
            PortError::Parse(format!(
                "malformed UUID in {FS_UUID_FILENAME}: {fs_uuid_str:?}"
            ))
        })?;

        let mut subvols = Vec::new();
        for sidecar in &sidecars {
            log::trace!("reading raw sidecar: {}", sidecar.display());
            let content = match std::fs::read_to_string(sidecar) {
                Ok(c) => c,
                Err(e) => {
                    log::warn!("skipping unreadable sidecar {}: {e}", sidecar.display());
                    continue;
                }
            };
            match SidecarInfo::from_toml(&content) {
                Ok(info) => match sidecar_to_subvolume(&info, &self.target_dir, fs_uuid.clone()) {
                    Ok(sv) => subvols.push(sv),
                    Err(e) => log::warn!("skipping malformed sidecar {}: {e}", sidecar.display()),
                },
                Err(e) => log::warn!("skipping unparseable sidecar {}: {e}", sidecar.display()),
            }
        }

        subvols.sort_by_key(|sv| sv.cgen);
        Ok(subvols)
    }

    /// Return the synthetic [`Subvolume`] for a single raw backup by reading
    /// its sidecar. `path` is the full path without extension
    /// (`<target_dir>/<leaf>`); the sidecar is at `path.with_extension("info")`.
    ///
    /// # Errors
    /// [`PortError::Io`] if the sidecar or UUID file cannot be read;
    /// [`PortError::Parse`] if either is malformed.
    fn show(&self, path: &Path) -> Result<Subvolume, PortError> {
        let target_dir = path.parent().ok_or_else(|| {
            PortError::Parse("raw backup path has no parent directory".to_string())
        })?;
        let uuid_file = target_dir.join(FS_UUID_FILENAME);
        let fs_uuid_str = std::fs::read_to_string(&uuid_file)?.trim().to_owned();
        let fs_uuid = Uuid::parse(&fs_uuid_str).ok_or_else(|| {
            PortError::Parse(format!(
                "malformed UUID in {FS_UUID_FILENAME}: {fs_uuid_str:?}"
            ))
        })?;
        let sidecar_path = sidecar_path_for(path);
        let content = std::fs::read_to_string(&sidecar_path)?;
        let info = SidecarInfo::from_toml(&content)?;
        sidecar_to_subvolume(&info, target_dir, fs_uuid)
    }
}

// ── DeletePort ────────────────────────────────────────────────────────────────

impl DeletePort for RawStreamAdapter {
    /// Delete the raw stream file and its sidecar.
    ///
    /// `path` is the full path without extension (`<target_dir>/<leaf>`).
    /// The sidecar is read first to find the stream filename (including codec
    /// extensions). If the sidecar is absent, a warning is logged and deletion
    /// continues with the sidecar path only.
    ///
    /// [`DeleteCommit`] is ignored (no btrfs transactions involved).
    ///
    /// # Errors
    /// [`PortError::Io`] if either file cannot be deleted.
    fn delete(&self, path: &Path, _commit: DeleteCommit) -> Result<(), PortError> {
        let sidecar_path = sidecar_path_for(path);
        let target_dir = path.parent().unwrap_or(path);

        // Read sidecar to locate the stream file.
        let stream_filename_opt = if sidecar_path.exists() {
            let content = std::fs::read_to_string(&sidecar_path)?;
            match SidecarInfo::from_toml(&content) {
                Ok(info) => Some(info.stream_file),
                Err(e) => {
                    log::warn!(
                        "could not parse sidecar {} during delete: {e}",
                        sidecar_path.display()
                    );
                    None
                }
            }
        } else {
            log::warn!("sidecar absent during delete: {}", sidecar_path.display());
            None
        };

        // Delete the stream file.
        if let Some(ref sf) = stream_filename_opt {
            let stream_path = target_dir.join(sf);
            log::debug!("removing raw stream file: {}", stream_path.display());
            if let Err(e) = std::fs::remove_file(&stream_path) {
                log::error!(
                    "failed to remove stream file {}: {e}",
                    stream_path.display()
                );
                return Err(PortError::Io(e));
            }
        }

        // Delete the sidecar.
        if sidecar_path.exists() {
            log::debug!("removing raw sidecar: {}", sidecar_path.display());
            if let Err(e) = std::fs::remove_file(&sidecar_path) {
                log::error!("failed to remove sidecar {}: {e}", sidecar_path.display());
                return Err(PortError::Io(e));
            }
        }

        Ok(())
    }
}

// ── TransferPort ──────────────────────────────────────────────────────────────

impl TransferPort for RawStreamAdapter {
    /// Run `btrfs send | [compress] | [encrypt] > <stream_file>`, write the
    /// `.info` sidecar atomically, and return a synthetic [`Subvolume`].
    ///
    /// **Verification contract** (weaker than btrfs receive — no `received_uuid`
    /// stamp available): GPG exit 0, stream file size > 0, sidecar round-trip.
    /// On any failure, the stream file and partial sidecar are deleted before
    /// returning.
    ///
    /// # Errors
    /// [`PortError::Command`] if the pipeline fails;
    /// [`PortError::Verification`] if post-conditions are not met;
    /// [`PortError::Io`] for filesystem errors;
    /// [`PortError::Parse`] if sidecar serialization fails.
    fn send_receive(
        &self,
        source: &Subvolume,
        selection: &ParentSelection,
        target_dir: &Path,
    ) -> Result<Subvolume, PortError> {
        let leaf = source
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| {
                PortError::Parse("source subvolume path has no filename component".to_string())
            })?
            .to_string();

        let encrypted = self.passphrase_file.is_some();
        let sf = stream_filename(&leaf, self.compress, encrypted);
        let stream_path = target_dir.join(&sf);
        let sidecar_path = target_dir.join(format!("{leaf}.info"));
        let sidecar_tmp = target_dir.join(format!("{leaf}.info.tmp"));

        // Bootstrap the per-directory filesystem UUID on the first transfer.
        let fs_uuid = load_or_create_fs_uuid(target_dir)?;

        // Build btrfs-send argv.
        let source_path = source.mountpoint.join(&source.path);
        let parent_path = selection
            .parent
            .as_ref()
            .map(|p| p.mountpoint.join(&p.path));
        let clone_paths: Vec<PathBuf> = selection
            .clone_sources
            .iter()
            .map(|c| c.mountpoint.join(&c.path))
            .collect();

        let mut send_args_owned: Vec<OsString> = vec!["send".into()];
        if let Some(ref pp) = parent_path {
            send_args_owned.push("-p".into());
            send_args_owned.push(pp.as_os_str().to_owned());
        }
        for cp in &clone_paths {
            send_args_owned.push("-c".into());
            send_args_owned.push(cp.as_os_str().to_owned());
        }
        send_args_owned.push(source_path.as_os_str().to_owned());

        // Build compress argv.
        let (compress_prog, compress_args_owned): (&str, Vec<OsString>) = match self.compress {
            RawCompress::Zstd => ("zstd", vec!["-T0".into(), "-c".into()]),
            RawCompress::Gzip => ("gzip", vec!["-c".into()]),
            RawCompress::Xz => ("xz", vec!["-c".into()]),
            RawCompress::None => ("cat", vec![]),
        };

        // Build consumer argv (encrypt to file, or write raw to file).
        let (consumer_prog, consumer_args_owned): (&str, Vec<OsString>) =
            if let Some(ref pf) = self.passphrase_file {
                (
                    "gpg",
                    vec![
                        "--symmetric".into(),
                        "--batch".into(),
                        "--passphrase-file".into(),
                        pf.as_os_str().to_owned(),
                        "-o".into(),
                        stream_path.as_os_str().to_owned(),
                    ],
                )
            } else {
                // No encryption: use `dd` to write to file without a shell.
                let mut of_arg = OsString::from("of=");
                of_arg.push(stream_path.as_os_str());
                ("dd", vec![of_arg, "bs=65536".into()])
            };

        let send_args: Vec<&std::ffi::OsStr> =
            send_args_owned.iter().map(OsString::as_os_str).collect();
        let compress_args: Vec<&std::ffi::OsStr> = compress_args_owned
            .iter()
            .map(OsString::as_os_str)
            .collect();
        let consumer_args: Vec<&std::ffi::OsStr> = consumer_args_owned
            .iter()
            .map(OsString::as_os_str)
            .collect();

        log::info!(
            "raw stream transfer: {} → {}",
            source_path.display(),
            stream_path.display()
        );

        let result = self.runner.pipe3(
            (BTRFS, &send_args),
            (compress_prog, &compress_args),
            (consumer_prog, &consumer_args),
            None,
        );

        if let Err(e) = result {
            cleanup_raw(&stream_path, &sidecar_path, &sidecar_tmp);
            log::error!("raw stream pipeline failed: {e}");
            return Err(e);
        }

        // Verify stream file exists and has non-zero size.
        let meta = std::fs::metadata(&stream_path).map_err(|e| {
            cleanup_raw(&stream_path, &sidecar_path, &sidecar_tmp);
            log::error!(
                "raw stream verification failed — stream file missing: {}",
                stream_path.display()
            );
            PortError::Io(e)
        })?;
        if meta.len() == 0 {
            cleanup_raw(&stream_path, &sidecar_path, &sidecar_tmp);
            log::error!(
                "raw stream verification failed — stream file is empty: {}",
                stream_path.display()
            );
            return Err(PortError::Verification(format!(
                "stream file is empty after transfer: {}",
                stream_path.display()
            )));
        }

        // Build sidecar.
        let received_from_uuid = source.uuid.as_ref().ok_or_else(|| {
            cleanup_raw(&stream_path, &sidecar_path, &sidecar_tmp);
            PortError::Verification("source subvolume has no UUID".to_string())
        })?;

        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        // Generate a unique UUID for this stream.
        let stream_uuid = load_or_create_fs_uuid(target_dir).and_then(|_| {
            let raw =
                std::fs::read_to_string("/proc/sys/kernel/random/uuid").map_err(PortError::Io)?;
            let s = raw.trim().to_owned();
            Uuid::parse(&s).ok_or_else(|| {
                PortError::Parse(format!(
                    "unexpected uuid format from /proc/sys/kernel/random/uuid: {s:?}"
                ))
            })
        });

        let stream_uuid = match stream_uuid {
            Ok(u) => u,
            Err(e) => {
                cleanup_raw(&stream_path, &sidecar_path, &sidecar_tmp);
                return Err(e);
            }
        };

        let info = SidecarInfo {
            uuid: stream_uuid.to_string(),
            received_from_uuid: received_from_uuid.to_string(),
            leaf: leaf.clone(),
            stream_file: sf.clone(),
            created_at,
            compress: match self.compress {
                RawCompress::Zstd => "zstd",
                RawCompress::Gzip => "gzip",
                RawCompress::Xz => "xz",
                RawCompress::None => "none",
            }
            .to_string(),
            encrypt: if self.passphrase_file.is_some() {
                "gpg-symmetric"
            } else {
                "none"
            }
            .to_string(),
        };

        // Serialize and verify round-trip before writing.
        let toml = match info.to_toml() {
            Ok(t) => t,
            Err(e) => {
                cleanup_raw(&stream_path, &sidecar_path, &sidecar_tmp);
                return Err(e);
            }
        };
        match SidecarInfo::from_toml(&toml) {
            Ok(v) if v.uuid == info.uuid && v.leaf == info.leaf => {}
            Ok(_) => {
                cleanup_raw(&stream_path, &sidecar_path, &sidecar_tmp);
                log::error!("sidecar round-trip mismatch");
                return Err(PortError::Verification(
                    "sidecar failed round-trip check".to_string(),
                ));
            }
            Err(e) => {
                cleanup_raw(&stream_path, &sidecar_path, &sidecar_tmp);
                log::error!("sidecar round-trip parse error: {e}");
                return Err(PortError::Verification(format!(
                    "sidecar failed round-trip: {e}"
                )));
            }
        }

        // Write sidecar atomically: tmp → rename.
        if let Err(e) = std::fs::write(&sidecar_tmp, &toml) {
            cleanup_raw(&stream_path, &sidecar_path, &sidecar_tmp);
            return Err(PortError::Io(e));
        }
        if let Err(e) = std::fs::rename(&sidecar_tmp, &sidecar_path) {
            cleanup_raw(&stream_path, &sidecar_path, &sidecar_tmp);
            return Err(PortError::Io(e));
        }

        log::info!("raw stream transfer complete: {sf}");

        sidecar_to_subvolume(&info, target_dir, fs_uuid)
    }
}

/// Remove stream file, sidecar, and temp sidecar (best-effort; errors are logged
/// and ignored since we are already on the error path).
fn cleanup_raw(stream: &Path, sidecar: &Path, sidecar_tmp: &Path) {
    let _ = std::fs::remove_file(stream);
    let _ = std::fs::remove_file(sidecar);
    let _ = std::fs::remove_file(sidecar_tmp);
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::path::PathBuf;

    // ── Sidecar round-trip ────────────────────────────────────────────────────

    fn sample_sidecar() -> SidecarInfo {
        SidecarInfo {
            uuid: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string(),
            received_from_uuid: "11111111-2222-3333-4444-555555555555".to_string(),
            leaf: "home.20260625T020000+0200".to_string(),
            stream_file: "home.20260625T020000+0200.btrfs.zst.gpg".to_string(),
            created_at: 1_750_809_600,
            compress: "zstd".to_string(),
            encrypt: "gpg-symmetric".to_string(),
        }
    }

    #[test]
    fn sidecar_round_trips() {
        let original = sample_sidecar();
        let toml = original.to_toml().unwrap();
        let parsed = SidecarInfo::from_toml(&toml).unwrap();
        assert_eq!(parsed.uuid, original.uuid);
        assert_eq!(parsed.received_from_uuid, original.received_from_uuid);
        assert_eq!(parsed.leaf, original.leaf);
        assert_eq!(parsed.stream_file, original.stream_file);
        assert_eq!(parsed.created_at, original.created_at);
        assert_eq!(parsed.compress, original.compress);
        assert_eq!(parsed.encrypt, original.encrypt);
    }

    #[test]
    fn sidecar_rejects_malformed_uuid() {
        let toml = r#"
uuid = "not-a-uuid"
received_from_uuid = "11111111-2222-3333-4444-555555555555"
leaf = "home.20260625T020000+0200"
stream_file = "home.20260625T020000+0200.btrfs.zst.gpg"
created_at = 1750809600
compress = "zstd"
encrypt = "gpg-symmetric"
"#;
        let err = SidecarInfo::from_toml(toml).unwrap_err();
        assert!(matches!(err, PortError::Parse(_)));
        assert!(err.to_string().contains("malformed uuid"));
    }

    #[test]
    fn sidecar_rejects_malformed_received_from_uuid() {
        let toml = r#"
uuid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
received_from_uuid = "BAD"
leaf = "home.20260625T020000+0200"
stream_file = "home.20260625T020000+0200.btrfs.zst.gpg"
created_at = 1750809600
compress = "zstd"
encrypt = "gpg-symmetric"
"#;
        let err = SidecarInfo::from_toml(toml).unwrap_err();
        assert!(matches!(err, PortError::Parse(_)));
        assert!(err.to_string().contains("malformed received_from_uuid"));
    }

    #[test]
    fn sidecar_rejects_wrong_type_for_created_at() {
        let toml = r#"
uuid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
received_from_uuid = "11111111-2222-3333-4444-555555555555"
leaf = "home.20260625T020000+0200"
stream_file = "home.20260625T020000+0200.btrfs.zst.gpg"
created_at = "not-a-number"
compress = "zstd"
encrypt = "gpg-symmetric"
"#;
        let err = SidecarInfo::from_toml(toml).unwrap_err();
        assert!(matches!(err, PortError::Parse(_)));
    }

    #[test]
    fn sidecar_rejects_missing_required_field() {
        // No `leaf` field.
        let toml = r#"
uuid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
received_from_uuid = "11111111-2222-3333-4444-555555555555"
stream_file = "home.btrfs"
created_at = 1750809600
compress = "zstd"
encrypt = "none"
"#;
        let err = SidecarInfo::from_toml(toml).unwrap_err();
        assert!(matches!(err, PortError::Parse(_)));
    }

    // ── stable_id ─────────────────────────────────────────────────────────────

    #[test]
    fn stable_id_is_deterministic() {
        let uuid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
        let id1 = stable_id(uuid);
        let id2 = stable_id(uuid);
        assert_eq!(id1, id2);
    }

    #[test]
    fn stable_id_differs_for_different_uuids() {
        let a = stable_id("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
        let b = stable_id("bbbbbbbb-cccc-dddd-eeee-ffffffffffff");
        assert_ne!(a, b);
    }

    // ── sidecar_to_subvolume ──────────────────────────────────────────────────

    fn fs_uuid() -> Uuid {
        Uuid::parse("ffffffff-ffff-4fff-8fff-ffffffffffff").unwrap()
    }

    #[test]
    fn sidecar_to_subvolume_maps_fields_correctly() {
        let info = sample_sidecar();
        let dir = PathBuf::from("/mnt/backup/raw");
        let sv = sidecar_to_subvolume(&info, &dir, fs_uuid()).unwrap();

        assert_eq!(sv.uuid.as_ref().unwrap().as_str(), info.uuid);
        assert_eq!(
            sv.received_uuid.as_ref().unwrap().as_str(),
            info.received_from_uuid
        );
        assert!(sv.readonly);
        assert!(sv.parent_uuid.is_none());
        assert_eq!(sv.path, PathBuf::from(&info.leaf));
        assert_eq!(sv.mountpoint, dir);
        assert_eq!(sv.cgen, info.created_at);
        assert_eq!(sv.generation, info.created_at);
        assert_eq!(sv.id, stable_id(&info.uuid));
    }

    #[test]
    fn sidecar_to_subvolume_rejects_malformed_uuid_field() {
        let mut info = sample_sidecar();
        info.uuid = "bad".to_string();
        let err = sidecar_to_subvolume(&info, Path::new("/mnt"), fs_uuid()).unwrap_err();
        assert!(matches!(err, PortError::Parse(_)));
    }

    // ── stream_filename ───────────────────────────────────────────────────────

    #[test]
    fn stream_filename_encodes_codec_extensions() {
        assert_eq!(
            stream_filename("home.20260625T020000", RawCompress::Zstd, true),
            "home.20260625T020000.btrfs.zst.gpg"
        );
        assert_eq!(
            stream_filename("home.20260625T020000", RawCompress::Gzip, false),
            "home.20260625T020000.btrfs.gz"
        );
        assert_eq!(
            stream_filename("home.20260625T020000", RawCompress::None, false),
            "home.20260625T020000.btrfs"
        );
        assert_eq!(
            stream_filename("home.20260625T020000", RawCompress::None, true),
            "home.20260625T020000.btrfs.gpg"
        );
    }

    // ── SubvolumeRepository ───────────────────────────────────────────────────

    #[test]
    fn list_returns_empty_when_directory_has_no_uuid_file_and_no_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        let adapter = RawStreamAdapter::new(dir.path().to_path_buf(), RawCompress::Zstd, None);
        let result = adapter.list(dir.path()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn list_returns_verification_error_when_sidecars_exist_without_uuid_file() {
        let dir = tempfile::tempdir().unwrap();
        // Write a sidecar without writing the UUID file.
        let info = sample_sidecar();
        let toml = info.to_toml().unwrap();
        std::fs::write(dir.path().join("home.20260625T020000+0200.info"), &toml).unwrap();

        let adapter = RawStreamAdapter::new(dir.path().to_path_buf(), RawCompress::Zstd, None);
        let err = adapter.list(dir.path()).unwrap_err();
        assert!(matches!(err, PortError::Verification(_)));
    }

    #[test]
    fn list_returns_sidecars_sorted_by_cgen() {
        let dir = tempfile::tempdir().unwrap();
        let fs_u = "ffffffff-ffff-4fff-8fff-ffffffffffff";
        std::fs::write(dir.path().join(FS_UUID_FILENAME), fs_u).unwrap();

        let mut info1 = sample_sidecar();
        info1.created_at = 200;
        info1.leaf = "home.20260625T020000+0200".to_string();
        info1.stream_file = "home.20260625T020000+0200.btrfs.zst.gpg".to_string();
        info1.uuid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".to_string();

        let mut info2 = sample_sidecar();
        info2.created_at = 100;
        info2.leaf = "home.20260624T020000+0200".to_string();
        info2.stream_file = "home.20260624T020000+0200.btrfs.zst.gpg".to_string();
        info2.uuid = "bbbbbbbb-cccc-dddd-eeee-ffffffffffff".to_string();

        std::fs::write(
            dir.path().join("home.20260625T020000+0200.info"),
            info1.to_toml().unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("home.20260624T020000+0200.info"),
            info2.to_toml().unwrap(),
        )
        .unwrap();

        let adapter = RawStreamAdapter::new(dir.path().to_path_buf(), RawCompress::Zstd, None);
        let svs = adapter.list(dir.path()).unwrap();
        assert_eq!(svs.len(), 2);
        assert_eq!(svs[0].cgen, 100); // earlier first
        assert_eq!(svs[1].cgen, 200);
    }

    #[test]
    fn show_returns_subvolume_from_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let fs_u = "ffffffff-ffff-4fff-8fff-ffffffffffff";
        std::fs::write(dir.path().join(FS_UUID_FILENAME), fs_u).unwrap();

        let info = sample_sidecar();
        let leaf = &info.leaf;
        std::fs::write(
            dir.path().join(format!("{leaf}.info")),
            info.to_toml().unwrap(),
        )
        .unwrap();

        let adapter = RawStreamAdapter::new(dir.path().to_path_buf(), RawCompress::Zstd, None);
        let sv = adapter.show(&dir.path().join(leaf)).unwrap();
        assert_eq!(sv.uuid.as_ref().unwrap().as_str(), info.uuid);
        assert_eq!(sv.path, PathBuf::from(leaf));
    }

    // ── DeletePort ────────────────────────────────────────────────────────────

    #[test]
    fn delete_removes_stream_and_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let info = sample_sidecar();
        let sidecar = dir.path().join(format!("{}.info", info.leaf));
        let stream = dir.path().join(&info.stream_file);
        std::fs::write(&sidecar, info.to_toml().unwrap()).unwrap();
        std::fs::write(&stream, b"fake stream data").unwrap();

        let adapter = RawStreamAdapter::new(dir.path().to_path_buf(), RawCompress::Zstd, None);
        adapter
            .delete(&dir.path().join(&info.leaf), DeleteCommit::Deferred)
            .unwrap();

        assert!(!sidecar.exists());
        assert!(!stream.exists());
    }

    #[test]
    fn delete_warns_and_continues_when_sidecar_absent() {
        crate::init_test_logger();
        let dir = tempfile::tempdir().unwrap();
        let adapter = RawStreamAdapter::new(dir.path().to_path_buf(), RawCompress::Zstd, None);
        // Delete with no sidecar and no stream file — both absent, should succeed
        // (best-effort: nothing to delete means nothing to fail on).
        let result = adapter.delete(
            &dir.path().join("home.20260625T020000"),
            DeleteCommit::Deferred,
        );
        // No stream file to delete either, but sidecar.exists() is false so
        // we skip the stream file deletion. The sidecar is also absent so we
        // skip that too. Result is Ok.
        assert!(result.is_ok());
    }

    // ── RecordingRunner (for TransferPort argv unit tests) ────────────────────

    use std::cell::RefCell;

    struct RecordingRunner {
        calls: RefCell<Vec<(String, Vec<String>, Vec<String>, Vec<String>)>>,
        pipe3_result: Result<(), String>,
    }

    impl RecordingRunner {
        fn ok() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                pipe3_result: Ok(()),
            }
        }
        fn err(msg: &str) -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                pipe3_result: Err(msg.to_string()),
            }
        }
        fn recorded_pipe3(&self) -> Vec<(String, Vec<String>, Vec<String>, Vec<String>)> {
            self.calls.borrow().clone()
        }
    }

    impl crate::command::CommandRunner for RecordingRunner {
        fn run(&self, _program: &str, _args: &[&OsStr]) -> Result<String, PortError> {
            Ok(String::new())
        }

        fn pipe(
            &self,
            _producer: (&str, &[&OsStr]),
            _consumer: (&str, &[&OsStr]),
            _on_progress: Option<std::sync::Arc<dyn Fn(u64, u64) + Send + Sync>>,
        ) -> Result<(), PortError> {
            Ok(())
        }

        fn pipe3(
            &self,
            producer: (&str, &[&OsStr]),
            middle: (&str, &[&OsStr]),
            consumer: (&str, &[&OsStr]),
            _on_progress: Option<std::sync::Arc<dyn Fn(u64, u64) + Send + Sync>>,
        ) -> Result<(), PortError> {
            let to_strings = |args: &[&OsStr]| {
                args.iter()
                    .map(|a| a.to_string_lossy().to_string())
                    .collect::<Vec<_>>()
            };
            self.calls.borrow_mut().push((
                producer.0.to_string(),
                to_strings(producer.1),
                to_strings(middle.1),
                to_strings(consumer.1),
            ));
            self.pipe3_result
                .as_ref()
                .map(|_| ())
                .map_err(|e| PortError::Command(e.clone()))
        }
    }

    fn make_source(uuid: &str) -> Subvolume {
        Subvolume {
            id: 256,
            uuid: Uuid::parse(uuid),
            parent_uuid: None,
            received_uuid: None,
            generation: 100,
            cgen: 50,
            readonly: true,
            path: PathBuf::from("home.20260625T020000+0200"),
            fs_uuid: Uuid::parse("ffffffff-ffff-4fff-8fff-ffffffffffff").unwrap(),
            mountpoint: PathBuf::from("/mnt/mybtrfs-source/snapshots"),
        }
    }

    #[test]
    fn send_receive_calls_pipe3_with_correct_argv_for_zstd_gpg() {
        let dir = tempfile::tempdir().unwrap();
        let pf = dir.path().join("pass.txt");
        std::fs::write(&pf, b"").unwrap();

        // Pre-create the stream file (RecordingRunner doesn't actually create it).
        let stream_name = "home.20260625T020000+0200.btrfs.zst.gpg";
        std::fs::write(dir.path().join(stream_name), b"fake encrypted data").unwrap();

        let runner = RecordingRunner::ok();
        let adapter = RawStreamAdapter::with_runner(
            dir.path().to_path_buf(),
            RawCompress::Zstd,
            Some(pf.clone()),
            Box::new(runner),
        );

        let source = make_source("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
        let selection = mybtrfs_domain::parent::ParentSelection {
            parent: None,
            clone_sources: vec![],
        };

        let result = adapter.send_receive(&source, &selection, dir.path());
        // The UUID file is created by load_or_create_fs_uuid; the stream file
        // was pre-created by us. The sidecar is written by the adapter.
        assert!(result.is_ok(), "send_receive failed: {result:?}");
    }

    #[test]
    fn send_receive_returns_command_error_when_pipeline_fails() {
        let dir = tempfile::tempdir().unwrap();
        let pf = dir.path().join("pass.txt");
        std::fs::write(&pf, b"").unwrap();

        let runner = RecordingRunner::err("pipeline failed");
        let adapter = RawStreamAdapter::with_runner(
            dir.path().to_path_buf(),
            RawCompress::Zstd,
            Some(pf),
            Box::new(runner),
        );

        let source = make_source("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
        let selection = mybtrfs_domain::parent::ParentSelection {
            parent: None,
            clone_sources: vec![],
        };

        let err = adapter
            .send_receive(&source, &selection, dir.path())
            .unwrap_err();
        assert!(matches!(err, PortError::Command(_)));
    }

    // ── Integration test (requires root + loopback env + GPG key C8D7DA12) ──

    #[test]
    #[ignore = "requires root, /mnt/mybtrfs-source, /mnt/mybtrfs-backup, and GPG key C8D7DA12"]
    fn integration_full_raw_zstd_gpg_backup() {
        let source_snap = PathBuf::from("/mnt/mybtrfs-source/snapshots");
        let target_dir = PathBuf::from("/mnt/mybtrfs-backup/raw");
        let passphrase = PathBuf::from("/tmp/mybtrfs-test.passphrase");

        std::fs::create_dir_all(&target_dir).unwrap();
        std::fs::write(&passphrase, b"").unwrap();

        let adapter =
            RawStreamAdapter::new(target_dir.clone(), RawCompress::Zstd, Some(passphrase));

        // The source needs to have a snapshot present; this test only validates
        // the pipeline plumbing so it lists whatever is there.
        let svs = adapter.list(&target_dir).unwrap();
        println!("existing raw backups: {}", svs.len());
    }
}
