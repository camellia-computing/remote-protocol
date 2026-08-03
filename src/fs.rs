#[cfg(windows)]
use std::os::windows::prelude::*;
use std::{
    ffi::{OsStr, OsString},
    fmt::{Debug, Display},
    io::{Cursor, Read as _, Write as _},
    path::{Path, PathBuf},
    sync::atomic::{AtomicI32, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions as CapOpenOptions},
};
use serde_derive::{Deserialize, Serialize};
use serde_json::json;
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufStream as TokioBufStream},
};

use crate::{anyhow::anyhow, bail, get_version_number, message_proto::*, ResultType, Stream};
// https://doc.rust-lang.org/std/os/windows/fs/trait.MetadataExt.html
use crate::{
    compress::{compress, decompress},
    config::Config,
};

static NEXT_JOB_ID: AtomicI32 = AtomicI32::new(1);

pub fn get_next_job_id() -> i32 {
    NEXT_JOB_ID.fetch_add(1, Ordering::SeqCst)
}

pub fn update_next_job_id(id: i32) {
    NEXT_JOB_ID.store(id, Ordering::SeqCst);
}

pub fn read_dir(path: &Path, include_hidden: bool) -> ResultType<FileDirectory> {
    let mut dir = FileDirectory {
        path: get_string(path),
        ..Default::default()
    };
    #[cfg(windows)]
    if "/" == &get_string(path) {
        let drives = unsafe { winapi::um::fileapi::GetLogicalDrives() };
        for i in 0..32 {
            if drives & (1 << i) != 0 {
                let name = format!(
                    "{}:",
                    std::char::from_u32('A' as u32 + i as u32).unwrap_or('A')
                );
                dir.entries.push(FileEntry {
                    name,
                    entry_type: FileType::DirDrive.into(),
                    ..Default::default()
                });
            }
        }
        return Ok(dir);
    }
    for entry in path.read_dir()?.flatten() {
        let p = entry.path();
        let name = p
            .file_name()
            .map(|p| p.to_str().unwrap_or(""))
            .unwrap_or("")
            .to_owned();
        if name.is_empty() {
            continue;
        }
        let mut is_hidden = false;
        let meta;
        if let Ok(tmp) = std::fs::symlink_metadata(&p) {
            meta = tmp;
        } else {
            continue;
        }
        // docs.microsoft.com/en-us/windows/win32/fileio/file-attribute-constants
        #[cfg(windows)]
        if meta.file_attributes() & 0x2 != 0 {
            is_hidden = true;
        }
        #[cfg(not(windows))]
        if name.find('.').unwrap_or(usize::MAX) == 0 {
            is_hidden = true;
        }
        if is_hidden && !include_hidden {
            continue;
        }
        let (entry_type, size) = {
            if p.is_dir() {
                if meta.file_type().is_symlink() {
                    (FileType::DirLink.into(), 0)
                } else {
                    (FileType::Dir.into(), 0)
                }
            } else if meta.file_type().is_symlink() {
                (FileType::FileLink.into(), 0)
            } else {
                (FileType::File.into(), meta.len())
            }
        };
        let modified_time = meta
            .modified()
            .map(|x| {
                x.duration_since(std::time::SystemTime::UNIX_EPOCH)
                    .map(|x| x.as_secs())
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        dir.entries.push(FileEntry {
            name: get_file_name(&p),
            entry_type,
            is_hidden,
            size,
            modified_time,
            ..Default::default()
        });
    }
    Ok(dir)
}

#[inline]
pub fn get_file_name(p: &Path) -> String {
    p.file_name()
        .map(|p| p.to_str().unwrap_or(""))
        .unwrap_or("")
        .to_owned()
}

#[inline]
pub fn get_string(path: &Path) -> String {
    path.to_str().unwrap_or("").to_owned()
}

#[inline]
pub fn get_path(path: &str) -> PathBuf {
    Path::new(path).to_path_buf()
}

#[inline]
pub fn get_home_as_string() -> String {
    get_string(&Config::get_home())
}

fn read_dir_recursive(
    path: &Path,
    prefix: &Path,
    include_hidden: bool,
) -> ResultType<Vec<FileEntry>> {
    let mut files = Vec::new();
    if path.is_dir() {
        // to-do: symbol link handling, cp the link rather than the content
        // to-do: file mode, for unix
        let fd = read_dir(path, include_hidden)?;
        for entry in fd.entries.iter() {
            match entry.entry_type.enum_value() {
                Ok(FileType::File) => {
                    let mut entry = entry.clone();
                    entry.name = get_string(&prefix.join(entry.name));
                    files.push(entry);
                }
                Ok(FileType::Dir) => {
                    if let Ok(mut tmp) = read_dir_recursive(
                        &path.join(&entry.name),
                        &prefix.join(&entry.name),
                        include_hidden,
                    ) {
                        for entry in tmp.drain(0..) {
                            files.push(entry);
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(files)
    } else if path.is_file() {
        let (size, modified_time) = if let Ok(meta) = std::fs::metadata(path) {
            (
                meta.len(),
                meta.modified()
                    .map(|x| {
                        x.duration_since(std::time::SystemTime::UNIX_EPOCH)
                            .map(|x| x.as_secs())
                            .unwrap_or(0)
                    })
                    .unwrap_or(0),
            )
        } else {
            (0, 0)
        };
        files.push(FileEntry {
            entry_type: FileType::File.into(),
            size,
            modified_time,
            ..Default::default()
        });
        Ok(files)
    } else {
        bail!("Not exists");
    }
}

pub fn get_recursive_files(path: &str, include_hidden: bool) -> ResultType<Vec<FileEntry>> {
    read_dir_recursive(&get_path(path), &get_path(""), include_hidden)
}

fn read_empty_dirs_recursive(
    path: &Path,
    prefix: &Path,
    include_hidden: bool,
) -> ResultType<Vec<FileDirectory>> {
    let mut dirs = Vec::new();
    if path.is_dir() {
        // to-do: symbol link handling, cp the link rather than the content
        // to-do: file mode, for unix
        let fd = read_dir(path, include_hidden)?;
        if fd.entries.is_empty() {
            dirs.push(fd);
        } else {
            for entry in fd.entries.iter() {
                if let Ok(FileType::Dir) = entry.entry_type.enum_value() {
                    if let Ok(mut tmp) = read_empty_dirs_recursive(
                        &path.join(&entry.name),
                        &prefix.join(&entry.name),
                        include_hidden,
                    ) {
                        for entry in tmp.drain(0..) {
                            dirs.push(entry);
                        }
                    }
                }
            }
        }
        Ok(dirs)
    } else if path.is_file() {
        Ok(dirs)
    } else {
        bail!("Not exists");
    }
}

pub fn get_empty_dirs_recursive(
    path: &str,
    include_hidden: bool,
) -> ResultType<Vec<FileDirectory>> {
    read_empty_dirs_recursive(&get_path(path), &get_path(""), include_hidden)
}

#[inline]
pub fn is_file_exists(file_path: &str) -> bool {
    Path::new(file_path).exists()
}

#[inline]
pub fn can_enable_overwrite_detection(version: i64) -> bool {
    version >= get_version_number("1.1.10")
}

#[repr(i32)]
#[derive(Copy, Clone, Serialize, Debug, PartialEq, Default)]
pub enum JobType {
    #[default]
    Generic = 0,
    Printer = 1,
}

impl From<JobType> for file_transfer_send_request::FileType {
    fn from(t: JobType) -> Self {
        match t {
            JobType::Generic => file_transfer_send_request::FileType::Generic,
            JobType::Printer => file_transfer_send_request::FileType::Printer,
        }
    }
}

impl From<i32> for JobType {
    fn from(value: i32) -> Self {
        match value {
            0 => JobType::Generic,
            1 => JobType::Printer,
            _ => JobType::Generic,
        }
    }
}

impl From<JobType> for i32 {
    fn from(val: JobType) -> Self {
        val as i32
    }
}

impl JobType {
    pub fn from_proto(t: ::protobuf::EnumOrUnknown<file_transfer_send_request::FileType>) -> Self {
        match t.enum_value() {
            Ok(file_transfer_send_request::FileType::Generic) => JobType::Generic,
            Ok(file_transfer_send_request::FileType::Printer) => JobType::Printer,
            _ => JobType::Generic,
        }
    }
}

#[derive(Debug)]
pub enum DataSource {
    FilePath(PathBuf),
    MemoryCursor(Cursor<Vec<u8>>),
}

impl Default for DataSource {
    fn default() -> Self {
        DataSource::FilePath(PathBuf::new())
    }
}

impl serde::Serialize for DataSource {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            DataSource::FilePath(p) => serializer.serialize_str(p.to_str().unwrap_or("")),
            DataSource::MemoryCursor(_) => serializer.serialize_str(""),
        }
    }
}

impl Display for DataSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DataSource::FilePath(p) => write!(f, "File: {}", p.to_string_lossy()),
            DataSource::MemoryCursor(_) => write!(f, "Bytes"),
        }
    }
}

impl DataSource {
    fn to_meta(&self) -> String {
        match self {
            DataSource::FilePath(p) => p.to_string_lossy().to_string(),
            DataSource::MemoryCursor(_) => "".to_string(),
        }
    }
}

enum DataStream {
    FileStream(File),
    BufStream(TokioBufStream<Cursor<Vec<u8>>>),
}

#[derive(Debug)]
struct PendingWrite {
    parent: Dir,
    final_name: OsString,
    temp_name: String,
    digest_name: OsString,
    modified_time: u64,
}

impl Debug for DataStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DataStream::FileStream(fs) => write!(f, "{:?}", fs),
            DataStream::BufStream(_) => write!(f, "BufStream"),
        }
    }
}

impl DataStream {
    async fn write_all(&mut self, buf: &[u8]) -> ResultType<()> {
        match self {
            DataStream::FileStream(fs) => fs.write_all(buf).await?,
            DataStream::BufStream(bs) => bs.write_all(buf).await?,
        }
        Ok(())
    }

    async fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            DataStream::FileStream(fs) => fs.read(buf).await,
            DataStream::BufStream(bs) => bs.read(buf).await,
        }
    }
}

#[derive(Default, Serialize, Deserialize, Debug, Clone)]
pub struct FileDigest {
    pub size: u64,
    pub modified: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub temp_name: String,
}

#[derive(Default, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TransferJob {
    pub id: i32,
    pub r#type: JobType,
    pub remote: String,
    pub data_source: DataSource,
    pub show_hidden: bool,
    pub is_remote: bool,
    pub is_last_job: bool,
    pub is_resume: bool,
    pub file_num: i32,
    #[serde(skip_serializing)]
    files: Vec<FileEntry>,
    pub conn_id: i32, // server only

    #[serde(skip_serializing)]
    data_stream: Option<DataStream>,
    #[serde(skip_serializing)]
    pending_write: Option<PendingWrite>,
    #[serde(skip_serializing)]
    write_error: Option<String>,
    pub total_size: u64,
    finished_size: u64,
    transferred: u64,
    enable_overwrite_detection: bool,
    file_confirmed: bool,
    // indicating the last file is skipped
    file_skipped: bool,
    file_is_waiting: bool,
    default_overwrite_strategy: Option<bool>,
    #[serde(skip_serializing)]
    digest: FileDigest,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct TransferJobMeta {
    #[serde(default)]
    pub id: i32,
    #[serde(default)]
    pub remote: String,
    #[serde(default)]
    pub to: String,
    #[serde(default)]
    pub show_hidden: bool,
    #[serde(default)]
    pub file_num: i32,
    #[serde(default)]
    pub is_remote: bool,
}

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct RemoveJobMeta {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub is_remote: bool,
    #[serde(default)]
    pub no_confirm: bool,
}

#[inline]
fn get_ext(name: &str) -> &str {
    if let Some(i) = name.rfind('.') {
        return &name[i + 1..];
    }
    ""
}

#[inline]
fn is_compressed_file(name: &str) -> bool {
    let compressed_exts = ["xz", "gz", "zip", "7z", "rar", "bz2", "tgz", "png", "jpg"];
    let ext = get_ext(name);
    compressed_exts.contains(&ext)
}

pub fn validate_file_name_no_traversal(name: &str) -> ResultType<()> {
    if name.bytes().any(|b| b == 0) {
        bail!("file name contains null bytes");
    }
    let has_traversal = name
        .split(|c: char| c == '/' || (cfg!(windows) && c == '\\'))
        .filter(|s| !s.is_empty())
        .any(|s| s == "..");
    if has_traversal {
        bail!("path traversal detected in file name");
    }
    #[cfg(windows)]
    {
        if name.len() >= 2 {
            let bytes = name.as_bytes();
            if bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
                bail!("absolute path detected in file name");
            }
        }
        if name.starts_with('/') || name.starts_with('\\') {
            bail!("absolute path detected in file name");
        }
    }
    #[cfg(not(windows))]
    if name.starts_with('/') {
        bail!("absolute path detected in file name");
    }
    Ok(())
}

fn validate_transfer_file_names(files: &[FileEntry]) -> ResultType<()> {
    // Single-file transfer may use an empty relative name, because
    // the destination file path is carried by transfer metadata.
    if files.len() == 1 && files.first().is_some_and(|f| f.name.is_empty()) {
        return Ok(());
    }
    for file in files {
        if file.name.is_empty() {
            bail!("empty file name in multi-file transfer");
        }
        validate_file_name_no_traversal(&file.name)?;
    }
    Ok(())
}

#[inline]
fn validate_fs_path_argument(path: &str, arg_name: &str) -> ResultType<()> {
    if path.is_empty() {
        bail!("{arg_name} cannot be empty");
    }
    if path.bytes().any(|b| b == 0) {
        bail!("{arg_name} contains null bytes");
    }
    Ok(())
}

fn validate_no_symlink_components(base: &Path, name: &str) -> ResultType<()> {
    if name.is_empty() {
        return Ok(());
    }
    let mut current = base.to_path_buf();
    for component in Path::new(name).components() {
        match component {
            std::path::Component::Normal(seg) => {
                current.push(seg);
                // Best-effort guard: path-based checks are inherently TOCTOU-prone
                // if local filesystem state changes between validation and write.
                match std::fs::symlink_metadata(&current) {
                    Ok(meta) => {
                        // This is inherent to filesystem-based checks and acknowledged as a limitation.
                        // For true protection, you'd need openat(2) / O_NOFOLLOW at write time.
                        if meta.file_type().is_symlink() {
                            bail!("symlink path component is not allowed");
                        }
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        // Component does not exist yet, continue best-effort validation.
                    }
                    Err(err) => {
                        bail!(
                            "failed to validate path component '{}': {}",
                            current.display(),
                            err
                        );
                    }
                }
            }
            std::path::Component::CurDir => {}
            _ => {
                bail!("invalid file name component");
            }
        }
    }
    Ok(())
}

/// Validate an untrusted relative file name and existing path components before joining it.
pub fn join_validated_path(base: &Path, name: &str) -> ResultType<PathBuf> {
    validate_file_name_no_traversal(name)?;
    validate_no_symlink_components(base, name)?;
    Ok(TransferJob::join(base, name))
}

const DOWNLOAD_TEMP_PREFIX: &str = ".camellia-download-";
const DOWNLOAD_TEMP_SUFFIX: &str = ".part";
const DIGEST_TEMP_PREFIX: &str = ".camellia-digest-";
const DIGEST_TEMP_SUFFIX: &str = ".tmp";
const MAX_DIGEST_BYTES: u64 = 4096;
const RANDOM_NAME_ATTEMPTS: usize = 16;

fn append_suffix(name: &OsStr, suffix: &str) -> OsString {
    let mut result = name.to_os_string();
    result.push(suffix);
    result
}

fn random_sidecar_name(prefix: &str, suffix: &str) -> String {
    format!("{prefix}{}{suffix}", uuid::Uuid::new_v4())
}

fn validate_download_temp_name(name: &str) -> ResultType<()> {
    let Some(uuid) = name
        .strip_prefix(DOWNLOAD_TEMP_PREFIX)
        .and_then(|name| name.strip_suffix(DOWNLOAD_TEMP_SUFFIX))
    else {
        bail!("invalid transfer temporary file name");
    };
    if uuid.len() != 36 || uuid::Uuid::parse_str(uuid)?.hyphenated().to_string() != uuid {
        bail!("invalid transfer temporary file name");
    }
    Ok(())
}

fn nofollow_read_options() -> CapOpenOptions {
    let mut options = CapOpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    options
}

fn nofollow_write_options(create_new: bool) -> CapOpenOptions {
    let mut options = CapOpenOptions::new();
    options
        .write(true)
        .create_new(create_new)
        .follow(FollowSymlinks::No);
    options
}

fn open_ambient_directory_nofollow(path: &Path, create: bool) -> ResultType<Dir> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut root = PathBuf::new();
    let mut components = Vec::new();
    for component in absolute.components() {
        match component {
            std::path::Component::Prefix(prefix) => root.push(prefix.as_os_str()),
            std::path::Component::RootDir => {
                root.push(std::path::MAIN_SEPARATOR.to_string());
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if components.pop().is_none() {
                    bail!("directory path escapes its filesystem root");
                }
            }
            std::path::Component::Normal(component) => components.push(component.to_os_string()),
        }
    }
    if root.as_os_str().is_empty() {
        bail!("directory path has no filesystem root");
    }
    let mut directory = Dir::open_ambient_dir(root, ambient_authority())?;
    for component in components {
        directory = match directory.open_dir_nofollow(&component) {
            Ok(next) => next,
            Err(err) if create && err.kind() == std::io::ErrorKind::NotFound => {
                match directory.create_dir(&component) {
                    Ok(()) => {}
                    Err(create_err) if create_err.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(create_err) => return Err(create_err.into()),
                }
                directory.open_dir_nofollow(&component)?
            }
            Err(err) => return Err(err.into()),
        };
    }
    Ok(directory)
}

fn open_destination_parent(
    base: &Path,
    name: &str,
    create_directories: bool,
) -> ResultType<(Dir, OsString)> {
    validate_file_name_no_traversal(name)?;
    if name.is_empty() {
        let final_name = base
            .file_name()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| anyhow!("destination file name is empty"))?
            .to_os_string();
        let parent = base.parent().filter(|path| !path.as_os_str().is_empty());
        let parent = parent.unwrap_or_else(|| Path::new("."));
        return Ok((
            open_ambient_directory_nofollow(parent, create_directories)?,
            final_name,
        ));
    }

    let mut parent = open_ambient_directory_nofollow(base, create_directories)?;
    let components = Path::new(name)
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(component) => Some(Ok(component.to_os_string())),
            std::path::Component::CurDir => None,
            _ => Some(Err(anyhow!("invalid file name component"))),
        })
        .collect::<ResultType<Vec<_>>>()?;
    let (final_name, parent_components) = components
        .split_last()
        .ok_or_else(|| anyhow!("destination file name is empty"))?;

    for component in parent_components {
        parent = match parent.open_dir_nofollow(component) {
            Ok(directory) => directory,
            Err(err) if create_directories && err.kind() == std::io::ErrorKind::NotFound => {
                match parent.create_dir(component) {
                    Ok(()) => {}
                    Err(create_err) if create_err.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(create_err) => return Err(create_err.into()),
                }
                parent.open_dir_nofollow(component)?
            }
            Err(err) => return Err(err.into()),
        };
    }
    Ok((parent, final_name.clone()))
}

fn read_digest(parent: &Dir, digest_name: &OsStr) -> ResultType<Option<FileDigest>> {
    let file = match parent.open_with(digest_name, &nofollow_read_options()) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_DIGEST_BYTES {
        bail!("invalid transfer digest file");
    }
    let mut content = String::new();
    file.into_std()
        .take(MAX_DIGEST_BYTES + 1)
        .read_to_string(&mut content)?;
    if content.len() as u64 > MAX_DIGEST_BYTES {
        bail!("transfer digest file is too large");
    }
    Ok(Some(serde_json::from_str(&content)?))
}

fn write_digest_atomically(
    parent: &Dir,
    digest_name: &OsStr,
    digest: &FileDigest,
) -> ResultType<()> {
    let content = serde_json::to_vec(digest)?;
    if content.len() as u64 > MAX_DIGEST_BYTES {
        bail!("transfer digest file is too large");
    }
    for _ in 0..RANDOM_NAME_ATTEMPTS {
        let temp_name = random_sidecar_name(DIGEST_TEMP_PREFIX, DIGEST_TEMP_SUFFIX);
        let file = match parent.open_with(&temp_name, &nofollow_write_options(true)) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err.into()),
        };
        let mut file = file.into_std();
        let result = (|| -> ResultType<()> {
            file.write_all(&content)?;
            file.sync_all()?;
            parent.rename(&temp_name, parent, digest_name)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = parent.remove_file_or_symlink(&temp_name);
        }
        return result;
    }
    bail!("failed to allocate a unique transfer digest name")
}

fn remove_file_or_symlink_if_present(parent: &Dir, name: &OsStr) -> ResultType<()> {
    match parent.remove_file_or_symlink(name) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

fn ensure_safe_final_target(parent: &Dir, final_name: &OsStr) -> ResultType<()> {
    match parent.symlink_metadata(final_name) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("destination file is a symbolic link")
        }
        Ok(metadata) if !metadata.is_file() => bail!("destination is not a regular file"),
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

fn prepare_new_write(
    base: &Path,
    name: &str,
    digest: &FileDigest,
    modified_time: u64,
) -> ResultType<(std::fs::File, PendingWrite)> {
    let (parent, final_name) = open_destination_parent(base, name, true)?;
    ensure_safe_final_target(&parent, &final_name)?;
    let digest_name = append_suffix(&final_name, ".digest");
    let legacy_download_name = append_suffix(&final_name, ".download");

    if let Ok(Some(previous_digest)) = read_digest(&parent, &digest_name) {
        if validate_download_temp_name(&previous_digest.temp_name).is_ok() {
            remove_file_or_symlink_if_present(&parent, OsStr::new(&previous_digest.temp_name))?;
        }
    }
    remove_file_or_symlink_if_present(&parent, &legacy_download_name)?;

    for _ in 0..RANDOM_NAME_ATTEMPTS {
        let temp_name = random_sidecar_name(DOWNLOAD_TEMP_PREFIX, DOWNLOAD_TEMP_SUFFIX);
        let file = match parent.open_with(&temp_name, &nofollow_write_options(true)) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err.into()),
        };
        let digest = FileDigest {
            size: digest.size,
            modified: digest.modified,
            temp_name: temp_name.clone(),
        };
        if let Err(err) = write_digest_atomically(&parent, &digest_name, &digest) {
            let _ = parent.remove_file_or_symlink(&temp_name);
            return Err(err);
        }
        return Ok((
            file.into_std(),
            PendingWrite {
                parent,
                final_name,
                temp_name,
                digest_name,
                modified_time,
            },
        ));
    }
    bail!("failed to allocate a unique transfer temporary file name")
}

fn open_resumed_write(
    base: &Path,
    name: &str,
    digest: &FileDigest,
    modified_time: u64,
) -> ResultType<(std::fs::File, PendingWrite)> {
    let (parent, final_name) = open_destination_parent(base, name, false)?;
    ensure_safe_final_target(&parent, &final_name)?;
    let digest_name = append_suffix(&final_name, ".digest");
    let stored_digest = read_digest(&parent, &digest_name)?
        .ok_or_else(|| anyhow!("transfer digest file is missing"))?;
    if stored_digest.size != digest.size || stored_digest.modified != digest.modified {
        bail!("transfer digest changed before resume");
    }
    validate_download_temp_name(&stored_digest.temp_name)?;
    let file = parent.open_with(&stored_digest.temp_name, &nofollow_write_options(false))?;
    if !file.metadata()?.is_file() {
        bail!("transfer temporary path is not a regular file");
    }
    Ok((
        file.into_std(),
        PendingWrite {
            parent,
            final_name,
            temp_name: stored_digest.temp_name,
            digest_name,
            modified_time,
        },
    ))
}

impl TransferJob {
    #[allow(clippy::too_many_arguments)]
    pub fn new_write(
        id: i32,
        r#type: JobType,
        remote: String,
        data_source: DataSource,
        file_num: i32,
        show_hidden: bool,
        is_remote: bool,
        enable_overwrite_detection: bool,
    ) -> Self {
        log::info!("new write {}", data_source);
        Self {
            id,
            r#type,
            remote,
            data_source,
            file_num,
            show_hidden,
            is_remote,
            files: Vec::new(),
            total_size: 0,
            enable_overwrite_detection,
            ..Default::default()
        }
    }

    pub fn with_files(mut self, files: Vec<FileEntry>) -> ResultType<Self> {
        self.set_files(files)?;
        Ok(self)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_read(
        id: i32,
        r#type: JobType,
        remote: String,
        data_source: DataSource,
        file_num: i32,
        show_hidden: bool,
        is_remote: bool,
        enable_overwrite_detection: bool,
    ) -> ResultType<Self> {
        log::info!("new read {}", data_source);
        let (files, total_size) = match &data_source {
            DataSource::FilePath(p) => {
                let p = p.to_str().ok_or(anyhow!("Invalid path"))?;
                let files = get_recursive_files(p, show_hidden)?;
                let total_size = files.iter().map(|x| x.size).sum();
                (files, total_size)
            }
            DataSource::MemoryCursor(c) => (Vec::new(), c.get_ref().len() as u64),
        };
        Ok(Self {
            id,
            r#type,
            remote,
            data_source,
            file_num,
            show_hidden,
            is_remote,
            files,
            total_size,
            enable_overwrite_detection,
            ..Default::default()
        })
    }

    pub async fn get_buf_data(self) -> ResultType<Option<Vec<u8>>> {
        match self.data_stream {
            Some(DataStream::BufStream(mut bs)) => {
                bs.flush().await?;
                Ok(Some(bs.into_inner().into_inner()))
            }
            _ => Ok(None),
        }
    }

    #[inline]
    pub fn files(&self) -> &Vec<FileEntry> {
        &self.files
    }

    #[inline]
    pub fn set_files(&mut self, files: Vec<FileEntry>) -> ResultType<()> {
        validate_transfer_file_names(&files)?;
        if let DataSource::FilePath(base) = &self.data_source {
            for file in &files {
                validate_no_symlink_components(base, &file.name)?;
            }
        }
        self.total_size = files.iter().map(|x| x.size).sum();
        self.files = files;
        Ok(())
    }

    #[inline]
    pub fn set_digest(&mut self, size: u64, modified: u64) {
        self.digest.size = size;
        self.digest.modified = modified;
    }

    #[inline]
    pub fn id(&self) -> i32 {
        self.id
    }

    #[inline]
    pub fn total_size(&self) -> u64 {
        self.total_size
    }

    #[inline]
    pub fn finished_size(&self) -> u64 {
        self.finished_size
    }

    #[inline]
    pub fn transferred(&self) -> u64 {
        self.transferred
    }

    #[inline]
    pub fn file_num(&self) -> i32 {
        self.file_num
    }

    async fn finalize_pending_write(&mut self) -> ResultType<()> {
        let Some(pending) = self.pending_write.take() else {
            if let Some(DataStream::FileStream(file)) = self.data_stream.take() {
                file.sync_all().await?;
            }
            return Ok(());
        };
        let stream = self
            .data_stream
            .take()
            .ok_or_else(|| anyhow!("transfer file stream is missing"))?;
        let DataStream::FileStream(file) = stream else {
            bail!("transfer file stream has the wrong type");
        };
        file.sync_all().await?;
        let file = file.into_std().await;
        tokio::task::spawn_blocking(move || -> ResultType<()> {
            let modified_time = i64::try_from(pending.modified_time)
                .map_err(|_| anyhow!("file modification time is out of range"))?;
            filetime::set_file_handle_times(
                &file,
                None,
                Some(filetime::FileTime::from_unix_time(modified_time, 0)),
            )?;
            ensure_safe_final_target(&pending.parent, &pending.final_name)?;
            pending
                .parent
                .rename(&pending.temp_name, &pending.parent, &pending.final_name)?;
            remove_file_or_symlink_if_present(&pending.parent, &pending.digest_name)?;
            Ok(())
        })
        .await??;
        Ok(())
    }

    pub async fn modify_time(&mut self) -> ResultType<()> {
        if self.r#type == JobType::Printer {
            return Ok(());
        }
        self.finalize_pending_write().await
    }

    pub async fn remove_download_file(&mut self) -> ResultType<()> {
        if self.r#type == JobType::Printer {
            return Ok(());
        }
        self.data_stream.take();
        if let Some(pending) = self.pending_write.take() {
            tokio::task::spawn_blocking(move || -> ResultType<()> {
                remove_file_or_symlink_if_present(&pending.parent, OsStr::new(&pending.temp_name))?;
                remove_file_or_symlink_if_present(&pending.parent, &pending.digest_name)?;
                Ok(())
            })
            .await??;
            return Ok(());
        }

        let (base, name) = match &self.data_source {
            DataSource::FilePath(base) => {
                let file_num = self.file_num as usize;
                let Some(entry) = self.files.get(file_num) else {
                    return Ok(());
                };
                (base.clone(), entry.name.clone())
            }
            DataSource::MemoryCursor(_) => return Ok(()),
        };
        tokio::task::spawn_blocking(move || -> ResultType<()> {
            let (parent, final_name) = open_destination_parent(&base, &name, false)?;
            let digest_name = append_suffix(&final_name, ".digest");
            if let Ok(Some(digest)) = read_digest(&parent, &digest_name) {
                if validate_download_temp_name(&digest.temp_name).is_ok() {
                    remove_file_or_symlink_if_present(&parent, OsStr::new(&digest.temp_name))?;
                }
            }
            remove_file_or_symlink_if_present(&parent, &digest_name)?;
            remove_file_or_symlink_if_present(&parent, &append_suffix(&final_name, ".download"))?;
            Ok(())
        })
        .await??;
        Ok(())
    }

    #[inline]
    pub fn set_finished_size_on_resume(&mut self) {
        if self.is_resume && self.file_num > 0 {
            let finished_size: u64 = self
                .files
                .iter()
                .take(self.file_num as usize)
                .map(|file| file.size)
                .sum();
            self.finished_size = finished_size;
        }
    }

    pub async fn write(&mut self, block: FileTransferBlock) -> ResultType<()> {
        if block.id != self.id {
            bail!("Wrong id");
        }
        if let Some(err) = self.write_error.as_ref() {
            bail!("cannot continue file transfer after resume failure: {err}");
        }
        let file_num = block.file_num as usize;
        if matches!(self.data_source, DataSource::FilePath(_)) && file_num >= self.files.len() {
            bail!("Wrong file number");
        }
        let should_open = matches!(self.data_source, DataSource::FilePath(_))
            && (file_num != self.file_num as usize || self.data_stream.is_none());
        if should_open {
            self.finalize_pending_write().await?;
            self.file_num = block.file_num;
            let (base, entry) = match &self.data_source {
                DataSource::FilePath(base) => (base.clone(), self.files[file_num].clone()),
                DataSource::MemoryCursor(_) => bail!("file transfer source changed unexpectedly"),
            };
            if self.r#type == JobType::Printer {
                self.data_stream = Some(DataStream::FileStream(File::create(base).await?));
            } else {
                let digest = self.digest.clone();
                let name = entry.name;
                let modified_time = entry.modified_time;
                let (file, pending) = tokio::task::spawn_blocking(move || {
                    prepare_new_write(&base, &name, &digest, modified_time)
                })
                .await??;
                self.data_stream = Some(DataStream::FileStream(File::from_std(file)));
                self.pending_write = Some(pending);
            }
        } else if let DataSource::MemoryCursor(cursor) = &self.data_source {
            if self.data_stream.is_none() {
                self.data_stream = Some(DataStream::BufStream(TokioBufStream::new(cursor.clone())));
            }
        }
        if block.compressed {
            let tmp = decompress(&block.data);
            self.data_stream
                .as_mut()
                .ok_or(anyhow!("data stream is None"))?
                .write_all(&tmp)
                .await?;
            self.finished_size += tmp.len() as u64;
        } else {
            self.data_stream
                .as_mut()
                .ok_or(anyhow!("file is None"))?
                .write_all(&block.data)
                .await?;
            self.finished_size += block.data.len() as u64;
        }
        self.transferred += block.data.len() as u64;
        Ok(())
    }

    #[inline]
    pub fn join(p: &Path, name: &str) -> PathBuf {
        if name.is_empty() {
            p.to_path_buf()
        } else {
            p.join(name)
        }
    }

    /// Open the data stream for the current file.
    /// Returns Ok(true) if job is done, Ok(false) otherwise.
    async fn open_data_stream(&mut self) -> ResultType<bool> {
        let file_num = self.file_num as usize;
        match &mut self.data_source {
            DataSource::FilePath(p) => {
                if file_num >= self.files.len() {
                    // job done
                    self.data_stream.take();
                    return Ok(true);
                };
                if self.data_stream.is_none() {
                    match File::open(Self::join(p, &self.files[file_num].name)).await {
                        Ok(file) => {
                            self.data_stream = Some(DataStream::FileStream(file));
                            self.file_confirmed = false;
                            self.file_is_waiting = false;
                        }
                        // On open error, behave the same as validation failure: advance
                        // to next file and return the error.
                        Err(err) => {
                            self.file_num += 1;
                            self.file_confirmed = false;
                            self.file_is_waiting = false;
                            return Err(err.into());
                        }
                    }
                }
            }
            DataSource::MemoryCursor(c) => {
                if self.data_stream.is_none() {
                    let mut t = std::io::Cursor::new(Vec::new());
                    std::mem::swap(&mut t, c);
                    self.data_stream = Some(DataStream::BufStream(TokioBufStream::new(t)));
                }
            }
        }
        Ok(false)
    }

    /// Get current file's digest (last_modified, file_size) for overwrite detection.
    async fn get_current_digest(&self) -> ResultType<(u64, u64)> {
        let meta = match self.data_stream.as_ref().ok_or(anyhow!("file is None"))? {
            DataStream::FileStream(file) => file.metadata().await?,
            DataStream::BufStream(_) => bail!("No digest for buf stream"),
        };
        let last_modified = meta
            .modified()?
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_secs();
        Ok((last_modified, meta.len()))
    }

    async fn init_data_stream(&mut self, stream: &mut crate::Stream) -> ResultType<()> {
        if self.open_data_stream().await? {
            return Ok(());
        }
        if self.r#type == JobType::Generic
            && self.enable_overwrite_detection
            && !self.file_confirmed()
            && !self.file_is_waiting()
        {
            self.send_current_digest(stream).await?;
            self.set_file_is_waiting(true);
        }
        Ok(())
    }

    /// Initialize data stream for CM (Connection Manager) scenario.
    /// Returns digest info (last_modified, file_size) if overwrite detection is enabled,
    /// so caller can send it via IPC instead of network stream.
    /// Returns Ok(None) if job is done or already initialized.
    pub async fn init_data_stream_for_cm(&mut self) -> ResultType<Option<(u64, u64)>> {
        if self.open_data_stream().await? {
            return Ok(None);
        }
        // For overwrite detection, return digest info instead of sending via stream
        if self.r#type == JobType::Generic
            && self.enable_overwrite_detection
            && !self.file_confirmed()
            && !self.file_is_waiting()
        {
            let digest = self.get_current_digest().await?;
            self.set_file_is_waiting(true);
            return Ok(Some(digest));
        }
        Ok(None)
    }

    pub async fn read(&mut self) -> ResultType<Option<FileTransferBlock>> {
        if self.r#type == JobType::Generic
            && self.enable_overwrite_detection
            && !self.file_confirmed()
        {
            return Ok(None);
        }

        let file_num = self.file_num as usize;
        let name = match &self.data_source {
            DataSource::FilePath(p) => {
                if file_num >= self.files.len() {
                    self.data_stream.take();
                    return Ok(None);
                };
                if self.files.len() == 1 && self.files[file_num].name.is_empty() {
                    p.file_name()
                        .map(|p| p.to_str().unwrap_or(""))
                        .unwrap_or("")
                } else {
                    &self.files[file_num].name
                }
            }
            DataSource::MemoryCursor(..) => "",
        };
        const BUF_SIZE: usize = 128 * 1024;
        let mut buf: Vec<u8> = vec![0; BUF_SIZE];
        let mut compressed = false;
        let mut offset: usize = 0;
        loop {
            match self
                .data_stream
                .as_mut()
                .ok_or(anyhow!("data stream is None"))?
                .read(&mut buf[offset..])
                .await
            {
                Err(err) => {
                    self.file_num += 1;
                    self.data_stream = None;
                    self.file_confirmed = false;
                    self.file_is_waiting = false;
                    return Err(err.into());
                }
                Ok(n) => {
                    offset += n;
                    if n == 0 || offset == BUF_SIZE {
                        break;
                    }
                }
            }
        }
        unsafe { buf.set_len(offset) };
        if offset == 0 {
            if matches!(self.data_source, DataSource::MemoryCursor(_)) {
                self.data_stream.take();
                return Ok(None);
            }
            self.file_num += 1;
            self.data_stream = None;
            self.file_confirmed = false;
            self.file_is_waiting = false;
        } else {
            self.finished_size += offset as u64;
            if matches!(self.data_source, DataSource::FilePath(_)) && !is_compressed_file(name) {
                let tmp = compress(&buf);
                if tmp.len() < buf.len() {
                    buf = tmp;
                    compressed = true;
                }
            }
            self.transferred += buf.len() as u64;
        }
        Ok(Some(FileTransferBlock {
            id: self.id,
            file_num: file_num as _,
            data: buf.into(),
            compressed,
            ..Default::default()
        }))
    }

    // Only for generic job and file stream
    async fn send_current_digest(&mut self, stream: &mut Stream) -> ResultType<()> {
        let (last_modified, file_size) = self.get_current_digest().await?;
        let mut msg = Message::new();
        let mut resp = FileResponse::new();
        resp.set_digest(FileTransferDigest {
            id: self.id,
            file_num: self.file_num,
            last_modified,
            file_size,
            is_resume: self.is_resume,
            ..Default::default()
        });
        msg.set_file_response(resp);
        stream.send(&msg).await?;
        log::info!(
            "id: {}, file_num: {}, digest message is sent. waiting for confirm. msg: {:?}",
            self.id,
            self.file_num,
            msg
        );
        Ok(())
    }

    pub fn set_overwrite_strategy(&mut self, overwrite_strategy: Option<bool>) {
        self.default_overwrite_strategy = overwrite_strategy;
    }

    pub fn default_overwrite_strategy(&self) -> Option<bool> {
        self.default_overwrite_strategy
    }

    pub fn set_file_confirmed(&mut self, file_confirmed: bool) {
        log::info!("id: {}, file_confirmed: {}", self.id, file_confirmed);
        self.file_confirmed = file_confirmed;
        self.file_skipped = false;
    }

    pub fn set_file_is_waiting(&mut self, file_is_waiting: bool) {
        self.file_is_waiting = file_is_waiting;
    }

    #[inline]
    pub fn file_is_waiting(&self) -> bool {
        self.file_is_waiting
    }

    #[inline]
    pub fn file_confirmed(&self) -> bool {
        self.file_confirmed
    }

    /// Indicating whether the last file is skipped
    #[inline]
    pub fn file_skipped(&self) -> bool {
        self.file_skipped
    }

    /// Indicating whether the whole task is skipped
    #[inline]
    pub fn job_skipped(&self) -> bool {
        self.file_skipped() && self.files.len() == 1
    }

    /// Check whether the job is completed after `read` returns `None`
    /// This is a helper function which gives additional lifecycle when the job reads `None`.
    /// If returns `true`, it means we can delete the job automatically. `False` otherwise.
    ///
    /// [`Note`]
    /// Conditions:
    /// 1. Files are not waiting for confirmation by peers.
    #[inline]
    pub fn job_completed(&self) -> bool {
        // has no error, Condition 2
        !self.enable_overwrite_detection || (!self.file_confirmed && !self.file_is_waiting)
    }

    /// Get job error message, useful for getting status when job had finished
    pub fn job_error(&self) -> Option<String> {
        if self.job_skipped() {
            return Some("skipped".to_string());
        }
        None
    }

    pub fn set_file_skipped(&mut self) -> bool {
        log::debug!("skip file {} in job {}", self.file_num, self.id);
        self.data_stream.take();
        self.set_file_confirmed(false);
        self.set_file_is_waiting(false);
        self.file_num += 1;
        self.file_skipped = true;
        true
    }

    async fn set_stream_offset(&mut self, file_num: usize, offset: u64) -> ResultType<()> {
        let (base, entry) = match &self.data_source {
            DataSource::FilePath(base) => {
                let entry = self
                    .files
                    .get(file_num)
                    .ok_or_else(|| anyhow!("wrong file number for resume"))?;
                (base.clone(), entry.clone())
            }
            DataSource::MemoryCursor(_) => bail!("memory transfers cannot be resumed"),
        };
        let digest = self.digest.clone();
        let name = entry.name;
        let modified_time = entry.modified_time;
        let (file, pending) = tokio::task::spawn_blocking(move || {
            open_resumed_write(&base, &name, &digest, modified_time)
        })
        .await??;
        let file_len = file.metadata()?.len();
        if offset > file_len {
            bail!("resume offset exceeds temporary file length");
        }
        let mut file = File::from_std(file);
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        self.data_stream = Some(DataStream::FileStream(file));
        self.pending_write = Some(pending);
        self.transferred += offset;
        self.finished_size += offset;
        Ok(())
    }

    pub async fn confirm(&mut self, r: &FileTransferSendConfirmRequest) -> bool {
        if self.file_num() != r.file_num {
            // This branch will always be hit if:
            // 1. `confirm()` is called in `ui_cm_interface.rs`
            // 2. Not resuming
            //
            // It is ok. Because `confirm()` in `ui_cm_interface.rs` is only used for resuming.
            log::info!("file num truncated, ignoring");
        } else {
            match r.union {
                Some(file_transfer_send_confirm_request::Union::Skip(s)) => {
                    self.write_error = None;
                    if s {
                        self.set_file_skipped();
                    } else {
                        self.set_file_confirmed(true);
                    }
                }
                Some(file_transfer_send_confirm_request::Union::OffsetBlk(offset)) => {
                    self.set_file_confirmed(true);
                    self.write_error = None;
                    // If offset is greater than 0, we need to seek to the offset
                    if offset > 0 {
                        if let Err(err) = self
                            .set_stream_offset(r.file_num as usize, offset as u64)
                            .await
                        {
                            log::warn!("Failed to resume file transfer: {err}");
                            self.write_error = Some(err.to_string());
                            return false;
                        }
                    }
                }
                _ => {}
            }
        }
        true
    }

    #[inline]
    pub fn gen_meta(&self) -> TransferJobMeta {
        TransferJobMeta {
            id: self.id,
            remote: self.remote.to_string(),
            to: self.data_source.to_meta(),
            file_num: self.file_num,
            show_hidden: self.show_hidden,
            is_remote: self.is_remote,
        }
    }
}

#[inline]
pub fn new_error<T: std::string::ToString>(id: i32, err: T, file_num: i32) -> Message {
    let mut resp = FileResponse::new();
    resp.set_error(FileTransferError {
        id,
        error: err.to_string(),
        file_num,
        ..Default::default()
    });
    let mut msg_out = Message::new();
    msg_out.set_file_response(resp);
    msg_out
}

#[inline]
pub fn new_dir(id: i32, path: String, files: Vec<FileEntry>) -> Message {
    let mut resp = FileResponse::new();
    resp.set_dir(FileDirectory {
        id,
        path,
        entries: files,
        ..Default::default()
    });
    let mut msg_out = Message::new();
    msg_out.set_file_response(resp);
    msg_out
}

#[inline]
pub fn new_block(block: FileTransferBlock) -> Message {
    let mut resp = FileResponse::new();
    resp.set_block(block);
    let mut msg_out = Message::new();
    msg_out.set_file_response(resp);
    msg_out
}

#[inline]
pub fn new_send_confirm(r: FileTransferSendConfirmRequest) -> Message {
    let mut msg_out = Message::new();
    let mut action = FileAction::new();
    action.set_send_confirm(r);
    msg_out.set_file_action(action);
    msg_out
}

#[inline]
pub fn new_receive(
    id: i32,
    path: String,
    file_num: i32,
    files: Vec<FileEntry>,
    total_size: u64,
) -> Message {
    let mut action = FileAction::new();
    action.set_receive(FileTransferReceiveRequest {
        id,
        path,
        files,
        file_num,
        total_size,
        ..Default::default()
    });
    let mut msg_out = Message::new();
    msg_out.set_file_action(action);
    msg_out
}

#[inline]
pub fn new_send(
    id: i32,
    r#type: JobType,
    path: String,
    file_num: i32,
    include_hidden: bool,
) -> Message {
    log::info!("new send: {}, id: {}", path, id);
    let mut action = FileAction::new();
    let t: file_transfer_send_request::FileType = r#type.into();
    action.set_send(FileTransferSendRequest {
        id,
        path,
        include_hidden,
        file_num,
        file_type: t.into(),
        ..Default::default()
    });
    let mut msg_out = Message::new();
    msg_out.set_file_action(action);
    msg_out
}

#[inline]
pub fn new_done(id: i32, file_num: i32) -> Message {
    let mut resp = FileResponse::new();
    resp.set_done(FileTransferDone {
        id,
        file_num,
        ..Default::default()
    });
    let mut msg_out = Message::new();
    msg_out.set_file_response(resp);
    msg_out
}

#[inline]
pub fn remove_job(id: i32, jobs: &mut Vec<TransferJob>) -> Option<TransferJob> {
    jobs.iter()
        .position(|x| x.id() == id)
        .map(|index| jobs.remove(index))
}

#[inline]
pub fn get_job(id: i32, jobs: &mut [TransferJob]) -> Option<&mut TransferJob> {
    jobs.iter_mut().find(|x| x.id() == id)
}

#[inline]
pub fn get_job_immutable(id: i32, jobs: &[TransferJob]) -> Option<&TransferJob> {
    jobs.iter().find(|x| x.id() == id)
}

async fn init_jobs(jobs: &mut [TransferJob], stream: &mut crate::Stream) -> ResultType<()> {
    for job in jobs.iter_mut() {
        if job.is_last_job {
            continue;
        }
        if let Err(err) = job.init_data_stream(stream).await {
            stream
                .send(&new_error(job.id(), err, job.file_num()))
                .await?;
        }
    }
    Ok(())
}

pub async fn handle_read_jobs(
    jobs: &mut Vec<TransferJob>,
    stream: &mut crate::Stream,
) -> ResultType<String> {
    init_jobs(jobs, stream).await?;

    let mut job_log = Default::default();
    let mut finished = Vec::new();
    for job in jobs.iter_mut() {
        if job.is_last_job {
            continue;
        }
        match job.read().await {
            Err(err) => {
                stream
                    .send(&new_error(job.id(), err, job.file_num()))
                    .await?;
            }
            Ok(Some(block)) => {
                stream.send(&new_block(block)).await?;
            }
            Ok(None) => {
                if job.job_completed() {
                    job_log = serialize_transfer_job(job, true, false, "");
                    finished.push(job.id());
                    match job.job_error() {
                        Some(err) => {
                            job_log = serialize_transfer_job(job, false, false, &err);
                            stream
                                .send(&new_error(job.id(), err, job.file_num()))
                                .await?
                        }
                        None => stream.send(&new_done(job.id(), job.file_num())).await?,
                    }
                } else {
                    // waiting confirmation.
                }
            }
        }
        // Break to handle jobs one by one.
        break;
    }
    for id in finished {
        let _ = remove_job(id, jobs);
    }
    Ok(job_log)
}

pub fn remove_all_empty_dir(path: &Path) -> ResultType<()> {
    let fd = read_dir(path, true)?;
    for entry in fd.entries.iter() {
        match entry.entry_type.enum_value() {
            Ok(FileType::Dir) => {
                remove_all_empty_dir(&path.join(&entry.name)).ok();
            }
            Ok(FileType::DirLink) | Ok(FileType::FileLink) => {
                std::fs::remove_file(path.join(&entry.name)).ok();
            }
            _ => {}
        }
    }
    std::fs::remove_dir(path).ok();
    Ok(())
}

#[inline]
pub fn remove_file(file: &str) -> ResultType<()> {
    validate_fs_path_argument(file, "file path")?;
    std::fs::remove_file(get_path(file))?;
    Ok(())
}

#[inline]
pub fn create_dir(dir: &str) -> ResultType<()> {
    validate_fs_path_argument(dir, "directory path")?;
    std::fs::create_dir_all(get_path(dir))?;
    Ok(())
}

#[inline]
pub fn rename_file(path: &str, new_name: &str) -> ResultType<()> {
    validate_fs_path_argument(path, "path")?;
    if new_name.is_empty() {
        bail!("new file name cannot be empty");
    }
    validate_file_name_no_traversal(new_name)?;
    let path = std::path::Path::new(&path);
    if path.exists() {
        let dir = path
            .parent()
            .ok_or(anyhow!("Parent directoy of {path:?} not exists"))?;
        let new_path = dir.join(new_name);
        std::fs::rename(path, &new_path)?;
        Ok(())
    } else {
        bail!("{path:?} not exists");
    }
}

#[inline]
pub fn transform_windows_path(entries: &mut Vec<FileEntry>) {
    for entry in entries {
        entry.name = entry.name.replace('\\', "/");
    }
}

pub enum DigestCheckResult {
    IsSame,
    NeedConfirm(FileTransferDigest),
    NoSuchFile,
}

#[inline]
pub fn is_write_need_confirmation(
    is_resume: bool,
    file_path: &str,
    digest: &FileTransferDigest,
) -> ResultType<DigestCheckResult> {
    let (parent, final_name) = match open_destination_parent(Path::new(file_path), "", false) {
        Ok(location) => location,
        Err(err)
            if err
                .downcast_ref::<std::io::Error>()
                .is_some_and(|err| err.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(DigestCheckResult::NoSuchFile);
        }
        Err(err) => return Err(err),
    };
    if is_resume {
        let digest_name = append_suffix(&final_name, ".digest");
        if let Some(local_digest) = read_digest(&parent, &digest_name)? {
            let is_identical = local_digest.modified == digest.last_modified
                && local_digest.size == digest.file_size;
            if is_identical {
                validate_download_temp_name(&local_digest.temp_name)?;
                let download =
                    parent.open_with(&local_digest.temp_name, &nofollow_read_options())?;
                let download_metadata = download.metadata()?;
                if !download_metadata.is_file() {
                    bail!("transfer temporary path is not a regular file");
                }
                let transferred_size = download_metadata.len();
                if transferred_size > 0 {
                    return Ok(DigestCheckResult::NeedConfirm(FileTransferDigest {
                        id: digest.id,
                        file_num: digest.file_num,
                        last_modified: digest.last_modified,
                        file_size: digest.file_size,
                        is_identical,
                        transferred_size,
                        ..Default::default()
                    }));
                }
            }
        }
    }

    match parent.symlink_metadata(&final_name) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("destination file is a symbolic link")
        }
        Ok(metadata) if metadata.is_file() => {
            let modified_time = metadata.modified()?;
            let remote_mt = Duration::from_secs(digest.last_modified);
            let local_mt = modified_time.into_std().duration_since(UNIX_EPOCH)?;
            // [Note]
            // We decide to give the decision whether to override the existing file to users,
            // which obey the behavior of the file manager in our system.
            let mut is_identical = false;
            if remote_mt == local_mt && digest.file_size == metadata.len() {
                is_identical = true;
            }
            Ok(DigestCheckResult::NeedConfirm(FileTransferDigest {
                id: digest.id,
                file_num: digest.file_num,
                last_modified: local_mt.as_secs(),
                file_size: metadata.len(),
                is_identical,
                ..Default::default()
            }))
        }
        Ok(_) => Ok(DigestCheckResult::NoSuchFile),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(DigestCheckResult::NoSuchFile),
        Err(err) => Err(err.into()),
    }
}

pub fn serialize_transfer_jobs(jobs: &[TransferJob]) -> String {
    let mut v = vec![];
    for job in jobs {
        let value = serde_json::to_value(job).unwrap_or_default();
        v.push(value);
    }
    serde_json::to_string(&v).unwrap_or_default()
}

pub fn serialize_transfer_job(job: &TransferJob, done: bool, cancel: bool, error: &str) -> String {
    let mut value = serde_json::to_value(job).unwrap_or_default();
    value["done"] = json!(done);
    value["cancel"] = json!(cancel);
    value["error"] = json!(error);
    serde_json::to_string(&value).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestTempDir {
        path: PathBuf,
    }

    impl TestTempDir {
        fn new(prefix: &str) -> Self {
            Self {
                path: unique_temp_dir(prefix),
            }
        }

        fn join(&self, path: &str) -> PathBuf {
            self.path.join(path)
        }
    }

    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("{}_{}_{}", prefix, std::process::id(), timestamp))
    }

    fn new_file_entry(name: &str) -> FileEntry {
        let mut entry = FileEntry::new();
        entry.name = name.to_string();
        entry
    }

    fn new_validation_job(id: i32) -> TransferJob {
        TransferJob::new_write(
            id,
            JobType::Generic,
            "/fake/remote".to_string(),
            DataSource::FilePath(std::env::temp_dir().join(format!("rustdesk_validation_{id}"))),
            0,
            false,
            true,
            false,
        )
    }

    fn new_write_job(id: i32, download_dir: PathBuf, name: &str) -> ResultType<TransferJob> {
        let job = TransferJob::new_write(
            id,
            JobType::Generic,
            "/fake/remote".to_string(),
            DataSource::FilePath(download_dir),
            0,
            false,
            true,
            false,
        )
        .with_files(vec![new_file_entry(name)])?;
        Ok(job)
    }

    fn transfer_block(id: i32, file_num: i32, data: &[u8]) -> FileTransferBlock {
        FileTransferBlock {
            id,
            file_num,
            data: data.to_vec().into(),
            ..Default::default()
        }
    }

    fn transfer_digest(id: i32, file_size: u64, last_modified: u64) -> FileTransferDigest {
        FileTransferDigest {
            id,
            file_num: 0,
            file_size,
            last_modified,
            ..Default::default()
        }
    }

    fn stored_digest(downloads: &Path, name: &str) -> FileDigest {
        let content = std::fs::read_to_string(downloads.join(format!("{name}.digest")))
            .expect("read stored transfer digest");
        serde_json::from_str(&content).expect("parse stored transfer digest")
    }

    #[cfg(unix)]
    fn create_test_file_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn create_test_file_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(target, link)
    }

    #[cfg(unix)]
    fn create_test_dir_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn create_test_dir_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }

    fn assert_err_contains(err: anyhow::Error, expected: &str) {
        assert!(
            err.to_string().contains(expected),
            "expected error containing '{}', got: {}",
            expected,
            err
        );
    }

    #[test]
    fn path_traversal_e2e_write_rejects_relative_escape() {
        let tmp_root = TestTempDir::new("rustdesk_e2e_relative");
        let downloads = tmp_root.join("downloads");
        std::fs::create_dir_all(&downloads).expect("create downloads dir");

        let err = new_write_job(1, downloads, "../traversal_proof.txt")
            .expect_err("relative path traversal must be rejected");
        assert_err_contains(err, "path traversal");
        assert!(!tmp_root.join("traversal_proof.txt").exists());
    }

    #[test]
    fn path_traversal_e2e_write_rejects_absolute_path() {
        let tmp_root = TestTempDir::new("rustdesk_e2e_absolute");
        let downloads = tmp_root.join("downloads");
        let absolute_target = tmp_root.join("fake_ssh").join("authorized_keys");
        std::fs::create_dir_all(&downloads).expect("create downloads dir");

        let err = new_write_job(2, downloads, &absolute_target.to_string_lossy())
            .expect_err("absolute path must be rejected");
        assert_err_contains(err, "absolute path");
        assert!(!absolute_target.exists());
    }

    #[test]
    #[cfg_attr(windows, ignore = "requires symlink privilege to create test symlink")]
    fn path_traversal_e2e_write_rejects_symlink_escape() {
        let tmp_root = TestTempDir::new("rustdesk_e2e_symlink");
        let downloads = tmp_root.join("downloads");
        let outside = tmp_root.join("outside");
        let escaped_target = outside.join("escape.txt");
        std::fs::create_dir_all(&downloads).expect("create downloads dir");
        std::fs::create_dir_all(&outside).expect("create outside dir");

        let symlink_path = downloads.join("link");
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(&outside, &symlink_path).expect("create symlink for test");
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::symlink_dir;
            symlink_dir(&outside, &symlink_path).expect("create directory symlink for test");
        }

        let err = new_write_job(3, downloads, "link/escape.txt")
            .expect_err("symlink traversal must be rejected");
        assert_err_contains(err, "symlink");
        assert!(!escaped_target.exists());
    }

    #[cfg(any(unix, windows))]
    #[cfg_attr(windows, ignore = "requires Windows symbolic-link privilege")]
    #[tokio::test]
    async fn write_does_not_follow_preexisting_download_symlink() {
        let tmp_root = TestTempDir::new("camellia_download_symlink");
        let downloads = tmp_root.join("downloads");
        let sentinel = tmp_root.join("sentinel.txt");
        std::fs::create_dir_all(downloads.join("report.txt"))
            .expect("create destination directory which prevents legacy finalize");
        std::fs::write(&sentinel, b"sentinel must remain unchanged")
            .expect("create external sentinel");
        create_test_file_symlink(&sentinel, &downloads.join("report.txt.download"))
            .expect("create malicious download symlink");

        let mut job = new_write_job(106, downloads, "report.txt").expect("create write job");
        let result = job
            .write(FileTransferBlock {
                id: 106,
                file_num: 0,
                data: b"attacker-controlled payload".to_vec().into(),
                ..Default::default()
            })
            .await;

        assert_eq!(
            std::fs::read(&sentinel).expect("read external sentinel"),
            b"sentinel must remain unchanged"
        );
        assert!(result.is_err(), "a directory destination must be rejected");
    }

    #[cfg(any(unix, windows))]
    #[cfg_attr(windows, ignore = "requires Windows symbolic-link privilege")]
    #[tokio::test]
    async fn write_rejects_parent_symlink_added_after_file_list_validation() {
        let tmp_root = TestTempDir::new("camellia_parent_symlink_swap");
        let downloads = tmp_root.join("downloads");
        let outside = tmp_root.join("outside");
        std::fs::create_dir_all(&downloads).expect("create downloads directory");
        std::fs::create_dir_all(&outside).expect("create outside directory");
        let mut job =
            new_write_job(107, downloads.clone(), "nested/report.txt").expect("create write job");
        create_test_dir_symlink(&outside, &downloads.join("nested"))
            .expect("swap parent for a symlink");

        let result = job.write(transfer_block(107, 0, b"payload")).await;

        assert!(
            result.is_err(),
            "a symlink parent must be rejected at open time"
        );
        assert!(!outside.join("report.txt").exists());
        assert!(std::fs::read_dir(&outside)
            .expect("read outside directory")
            .next()
            .is_none());
    }

    #[cfg(any(unix, windows))]
    #[cfg_attr(windows, ignore = "requires Windows symbolic-link privilege")]
    #[tokio::test]
    async fn write_rejects_destination_root_swapped_for_symlink() {
        let tmp_root = TestTempDir::new("camellia_destination_root_swap");
        let downloads = tmp_root.join("downloads");
        let original_downloads = tmp_root.join("original-downloads");
        let outside = tmp_root.join("outside");
        std::fs::create_dir_all(&downloads).expect("create downloads directory");
        std::fs::create_dir_all(&outside).expect("create outside directory");
        let mut job =
            new_write_job(115, downloads.clone(), "report.txt").expect("create write job");
        std::fs::rename(&downloads, &original_downloads).expect("move authorized directory");
        create_test_dir_symlink(&outside, &downloads)
            .expect("replace destination root with symlink");

        let result = job.write(transfer_block(115, 0, b"payload")).await;

        assert!(
            result.is_err(),
            "a replaced destination root must be rejected at open time"
        );
        assert!(std::fs::read_dir(&outside)
            .expect("read outside directory")
            .next()
            .is_none());
        assert!(std::fs::read_dir(&original_downloads)
            .expect("read original destination directory")
            .next()
            .is_none());
    }

    #[tokio::test]
    async fn write_uses_unique_same_directory_temp_and_commits_atomically() {
        let tmp_root = TestTempDir::new("camellia_atomic_commit");
        let downloads = tmp_root.join("downloads");
        std::fs::create_dir_all(&downloads).expect("create downloads directory");
        std::fs::write(downloads.join("report.txt"), b"old contents")
            .expect("create old destination");
        let mut job =
            new_write_job(108, downloads.clone(), "report.txt").expect("create write job");
        job.files[0].modified_time = 1_700_000_000;
        job.set_digest(11, 1_700_000_000);

        job.write(transfer_block(108, 0, b"new payload"))
            .await
            .expect("write transfer payload");

        assert_eq!(
            std::fs::read(downloads.join("report.txt")).expect("read pre-commit destination"),
            b"old contents"
        );
        assert!(!downloads.join("report.txt.download").exists());
        let digest = stored_digest(&downloads, "report.txt");
        validate_download_temp_name(&digest.temp_name).expect("validate random temp name");
        assert!(downloads.join(&digest.temp_name).is_file());

        job.modify_time().await.expect("commit transfer");

        assert_eq!(
            std::fs::read(downloads.join("report.txt")).expect("read committed destination"),
            b"new payload"
        );
        assert!(!downloads.join(&digest.temp_name).exists());
        assert!(!downloads.join("report.txt.digest").exists());
    }

    #[tokio::test]
    async fn resume_reopens_only_recorded_regular_temp_without_truncation() {
        let tmp_root = TestTempDir::new("camellia_secure_resume");
        let downloads = tmp_root.join("downloads");
        std::fs::create_dir_all(&downloads).expect("create downloads directory");
        let modified = 1_700_000_001;
        let mut first =
            new_write_job(109, downloads.clone(), "report.txt").expect("create first job");
        first.files[0].modified_time = modified;
        first.set_digest(6, modified);
        first
            .write(transfer_block(109, 0, b"abc"))
            .await
            .expect("write first transfer segment");
        if let Some(DataStream::FileStream(file)) = first.data_stream.as_mut() {
            file.sync_all()
                .await
                .expect("settle partial transfer bytes");
        }
        let first_digest = stored_digest(&downloads, "report.txt");
        drop(first);
        assert_eq!(first_digest.size, 6);
        assert_eq!(first_digest.modified, modified);
        assert_eq!(
            std::fs::metadata(downloads.join(&first_digest.temp_name))
                .expect("inspect partial transfer")
                .len(),
            3
        );

        let digest = transfer_digest(110, 6, modified);
        let confirmation =
            is_write_need_confirmation(true, &get_string(&downloads.join("report.txt")), &digest)
                .expect("inspect resumable transfer");
        let DigestCheckResult::NeedConfirm(confirmation) = confirmation else {
            panic!("regular partial transfer must request resume confirmation");
        };
        assert_eq!(confirmation.transferred_size, 3);

        let mut resumed =
            new_write_job(110, downloads.clone(), "report.txt").expect("create resumed job");
        resumed.files[0].modified_time = modified;
        resumed.set_digest(6, modified);
        assert!(
            resumed
                .confirm(&FileTransferSendConfirmRequest {
                    id: 110,
                    file_num: 0,
                    union: Some(file_transfer_send_confirm_request::Union::OffsetBlk(3)),
                    ..Default::default()
                })
                .await,
            "resume open must succeed"
        );
        resumed
            .write(transfer_block(110, 0, b"def"))
            .await
            .expect("append resumed segment");
        resumed
            .modify_time()
            .await
            .expect("commit resumed transfer");

        assert_eq!(
            std::fs::read(downloads.join("report.txt")).expect("read resumed destination"),
            b"abcdef"
        );
        assert!(!downloads.join(&first_digest.temp_name).exists());
        assert!(!downloads.join("report.txt.digest").exists());
    }

    #[tokio::test]
    async fn cancel_removes_only_owned_temp_and_digest() {
        let tmp_root = TestTempDir::new("camellia_secure_cancel");
        let downloads = tmp_root.join("downloads");
        std::fs::create_dir_all(&downloads).expect("create downloads directory");
        let mut job =
            new_write_job(111, downloads.clone(), "report.txt").expect("create write job");
        job.write(transfer_block(111, 0, b"partial"))
            .await
            .expect("write partial transfer");
        let digest = stored_digest(&downloads, "report.txt");

        job.remove_download_file()
            .await
            .expect("cancel transfer artifacts");

        assert!(!downloads.join(&digest.temp_name).exists());
        assert!(!downloads.join("report.txt.digest").exists());
        assert!(!downloads.join("report.txt").exists());
    }

    #[cfg(any(unix, windows))]
    #[cfg_attr(windows, ignore = "requires Windows symbolic-link privilege")]
    #[tokio::test]
    async fn resume_and_final_commit_reject_symlink_replacements() {
        let tmp_root = TestTempDir::new("camellia_resume_symlink_swap");
        let downloads = tmp_root.join("downloads");
        let sentinel = tmp_root.join("sentinel.txt");
        std::fs::create_dir_all(&downloads).expect("create downloads directory");
        std::fs::write(&sentinel, b"sentinel").expect("create sentinel");
        let modified = 1_700_000_002;
        let malicious_temp = random_sidecar_name(DOWNLOAD_TEMP_PREFIX, DOWNLOAD_TEMP_SUFFIX);
        std::fs::write(
            downloads.join("resume.txt.digest"),
            serde_json::to_vec(&FileDigest {
                size: 8,
                modified,
                temp_name: malicious_temp.clone(),
            })
            .expect("serialize malicious digest"),
        )
        .expect("write malicious digest");
        create_test_file_symlink(&sentinel, &downloads.join(&malicious_temp))
            .expect("replace resume temp with symlink");
        let mut resume_job =
            new_write_job(112, downloads.clone(), "resume.txt").expect("create resume job");
        resume_job.files[0].modified_time = modified;
        resume_job.set_digest(8, modified);

        assert!(
            resume_job.set_stream_offset(0, 1).await.is_err(),
            "resume must reject a symlink temp"
        );
        assert_eq!(
            std::fs::read(&sentinel).expect("read sentinel after resume rejection"),
            b"sentinel"
        );

        let mut commit_job =
            new_write_job(113, downloads.clone(), "commit.txt").expect("create commit job");
        commit_job
            .write(transfer_block(113, 0, b"new data"))
            .await
            .expect("write commit candidate");
        create_test_file_symlink(&sentinel, &downloads.join("commit.txt"))
            .expect("replace final target with symlink");

        assert!(
            commit_job.modify_time().await.is_err(),
            "commit must reject a symlink destination"
        );
        assert_eq!(
            std::fs::read(&sentinel).expect("read sentinel after commit rejection"),
            b"sentinel"
        );
    }

    #[cfg(any(unix, windows))]
    #[cfg_attr(windows, ignore = "requires Windows symbolic-link privilege")]
    #[tokio::test]
    async fn digest_symlink_is_replaced_without_following_it() {
        let tmp_root = TestTempDir::new("camellia_digest_symlink");
        let downloads = tmp_root.join("downloads");
        let sentinel = tmp_root.join("sentinel.txt");
        std::fs::create_dir_all(&downloads).expect("create downloads directory");
        std::fs::write(&sentinel, b"sentinel").expect("create sentinel");
        create_test_file_symlink(&sentinel, &downloads.join("report.txt.digest"))
            .expect("create digest symlink");
        let mut job =
            new_write_job(114, downloads.clone(), "report.txt").expect("create write job");

        job.write(transfer_block(114, 0, b"payload"))
            .await
            .expect("write through atomically replaced digest");

        assert_eq!(
            std::fs::read(&sentinel).expect("read digest sentinel"),
            b"sentinel"
        );
        assert!(
            std::fs::symlink_metadata(downloads.join("report.txt.digest"))
                .expect("inspect replaced digest")
                .is_file()
        );
        job.modify_time().await.expect("commit transfer");
    }

    #[test]
    fn set_files_allows_single_empty_name_for_single_file_transfer() {
        let mut job = new_validation_job(101);
        assert!(job.set_files(vec![new_file_entry("")]).is_ok());
    }

    #[test]
    fn set_files_rejects_empty_name_in_multi_file_transfer() {
        let mut job = new_validation_job(102);
        let err = job
            .set_files(vec![new_file_entry(""), new_file_entry("ok.txt")])
            .expect_err("empty name in multi-file transfer must be rejected");
        assert_err_contains(err, "empty file name");
    }

    #[test]
    fn set_files_rejects_null_byte_name() {
        let mut job = new_validation_job(103);
        let err = job
            .set_files(vec![new_file_entry("bad\0name.txt")])
            .expect_err("null byte in file name must be rejected");
        assert_err_contains(err, "null bytes");
    }

    #[test]
    fn set_files_rejects_mixed_entries_when_one_is_traversal() {
        let mut job = new_validation_job(104);
        let err = job
            .set_files(vec![
                new_file_entry("safe/file.txt"),
                new_file_entry("../../escape.txt"),
            ])
            .expect_err("any traversal entry must reject the full file list");
        assert_err_contains(err, "path traversal");
    }

    #[cfg(windows)]
    #[test]
    fn set_files_rejects_unc_absolute_path() {
        let mut job = new_validation_job(105);
        let err = job
            .set_files(vec![new_file_entry("\\\\server\\share\\payload.txt")])
            .expect_err("UNC absolute path must be rejected");
        assert_err_contains(err, "absolute path");
    }

    #[cfg(not(windows))]
    #[test]
    fn set_files_allows_backslash_prefixed_name_on_unix() {
        let mut job = new_validation_job(105);
        assert!(job
            .set_files(vec![new_file_entry("\\\\server\\share\\payload.txt")])
            .is_ok());
    }

    #[test]
    fn remove_file_rejects_empty_path() {
        let err = remove_file("").expect_err("empty file path must be rejected");
        assert_err_contains(err, "cannot be empty");
    }

    #[test]
    fn remove_file_rejects_null_byte_path() {
        let err = remove_file("bad\0path").expect_err("null byte path must be rejected");
        assert_err_contains(err, "null bytes");
    }

    #[test]
    fn create_dir_rejects_empty_path() {
        let err = create_dir("").expect_err("empty directory path must be rejected");
        assert_err_contains(err, "cannot be empty");
    }

    #[test]
    fn create_dir_rejects_null_byte_path() {
        let err = create_dir("bad\0path").expect_err("null byte path must be rejected");
        assert_err_contains(err, "null bytes");
    }

    #[test]
    fn rename_file_rejects_invalid_new_name() {
        let tmp_root = TestTempDir::new("rustdesk_rename_invalid");
        let src = tmp_root.join("source.txt");
        std::fs::create_dir_all(&tmp_root.path).expect("create temp dir");
        std::fs::write(&src, b"content").expect("create source file");

        let src_str = src.to_string_lossy().to_string();

        let err_empty =
            rename_file(&src_str, "").expect_err("empty new file name must be rejected");
        assert_err_contains(err_empty, "cannot be empty");

        let err_traversal = rename_file(&src_str, "../escape.txt")
            .expect_err("traversal new file name must be rejected");
        assert_err_contains(err_traversal, "path traversal");

        let err_null = rename_file(&src_str, "bad\0name.txt")
            .expect_err("null byte in new file name must be rejected");
        assert_err_contains(err_null, "null bytes");

        #[cfg(windows)]
        {
            let err_abs = rename_file(&src_str, "C:\\Windows\\Temp\\payload.txt")
                .expect_err("absolute new file name must be rejected");
            assert_err_contains(err_abs, "absolute path");
        }
        #[cfg(not(windows))]
        {
            let err_abs = rename_file(&src_str, "/tmp/payload.txt")
                .expect_err("absolute new file name must be rejected");
            assert_err_contains(err_abs, "absolute path");
        }
    }

    #[test]
    fn rename_file_accepts_valid_new_name() {
        let tmp_root = TestTempDir::new("rustdesk_rename_ok");
        let src = tmp_root.join("rename_src.txt");
        let dst = tmp_root.join("renamed.txt");
        std::fs::create_dir_all(&tmp_root.path).expect("create temp dir");
        std::fs::write(&src, b"content").expect("create source file");

        let src_str = src.to_string_lossy().to_string();
        rename_file(&src_str, "renamed.txt").expect("rename should succeed");

        assert!(!src.exists());
        assert!(dst.exists());
    }

    #[cfg(windows)]
    #[test]
    fn set_files_rejects_windows_drive_absolute_path() {
        let mut job = new_validation_job(106);
        let err = job
            .set_files(vec![new_file_entry("C:\\Windows\\Temp\\payload.txt")])
            .expect_err("drive-letter absolute path must be rejected");
        assert_err_contains(err, "absolute path");
    }

    #[cfg(windows)]
    #[test]
    fn set_files_rejects_windows_verbatim_drive_absolute_path() {
        let mut job = new_validation_job(1061);
        let err = job
            .set_files(vec![new_file_entry(r"\\?\C:\Windows\Temp\x.txt")])
            .expect_err("verbatim drive absolute path must be rejected");
        assert_err_contains(err, "absolute path");
    }
}
