//! Atomic, portable project storage. Blob paths are never read from the document.
use crate::{MAX_PROJECT_BYTES, Project};
use rayon::prelude::*;
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::File,
    io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::SystemTime,
};

pub const MAGIC: &[u8; 8] = b"MPRJ0001";
const MANIFEST_LIMIT: u64 = 64 * 1024 * 1024;
const IN_MEMORY_MANIFEST_LIMIT: u64 = 8 * 1024 * 1024;
const BLOB_LIMIT: u64 = 512 * 1024 * 1024;
const TOTAL_LIMIT: u64 = 4 * 1024 * 1024 * 1024;
const HEADER_BYTES: u64 = 48;
const TRANSFER_BYTES: u64 = 256 * 1024;
const EDIT_SOURCE_CHANGED: &str = "The source project changed while the action was running";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest<Workspace = serde_json::Value> {
    pub format: String,
    pub schema: u32,
    pub recipe: Project,
    #[serde(
        deserialize_with = "Deserialize::deserialize",
        bound(deserialize = "Workspace: Deserialize<'de>")
    )]
    pub workspace: Workspace,
    pub blobs: Vec<Blob>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Blob {
    pub bytes: u64,
    pub sha256: String,
}
#[derive(Clone)]
pub enum Source {
    File(Arc<tempfile::TempPath>),
    Retained(RetainedFile),
    Bytes(Arc<[u8]>),
}
impl Source {
    fn stamp(&self) -> Result<SourceStamp, String> {
        match self {
            Self::File(path) => SourceStamp::file(std::fs::metadata(path.as_ref())),
            Self::Retained(file) => Ok(file.stamp),
            Self::Bytes(bytes) => Ok(SourceStamp {
                bytes: bytes.len() as u64,
                modified: None,
            }),
        }
    }
    pub fn same_allocation(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::File(a), Self::File(b)) => Arc::ptr_eq(a, b),
            (Self::Retained(a), Self::Retained(b)) => a.same_allocation(b),
            (Self::Bytes(a), Self::Bytes(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}
/// A session file whose length and modification time were recorded when accepted.
#[derive(Clone)]
pub struct RetainedFile {
    path: Arc<tempfile::TempPath>,
    stamp: SourceStamp,
}

impl RetainedFile {
    pub fn new(path: Arc<tempfile::TempPath>) -> Result<Self, String> {
        let stamp = SourceStamp::file(std::fs::metadata(path.as_ref()))?;
        Ok(Self { path, stamp })
    }

    pub fn same_allocation(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.path, &other.path) && self.stamp == other.stamp
    }

    pub fn byte_len(&self) -> u64 {
        self.stamp.bytes
    }

    pub fn check(&self, file: &File) -> Result<(), String> {
        if SourceStamp::file(file.metadata())? != self.stamp {
            return Err("The retained map changed after it was accepted".into());
        }
        Ok(())
    }
}

impl AsRef<Path> for RetainedFile {
    fn as_ref(&self) -> &Path {
        self.path.as_ref().as_ref()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct SourceStamp {
    bytes: u64,
    modified: Option<SystemTime>,
}
impl SourceStamp {
    fn file(metadata: std::io::Result<std::fs::Metadata>) -> Result<Self, String> {
        let metadata = metadata.map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err("A retained map payload is not a file".into());
        }
        Ok(Self {
            bytes: metadata.len(),
            modified: Some(metadata.modified().map_err(|error| error.to_string())?),
        })
    }
}
pub struct Loaded<Workspace = serde_json::Value, Payload = Arc<tempfile::TempPath>> {
    pub manifest: Manifest<Workspace>,
    pub blobs: Vec<Payload>,
}
fn hash_valid(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl<W> Manifest<W> {
    fn validate(&self) -> Result<u64, String> {
        self.recipe.validate()?;
        if self.format != "mapper-project" || self.schema != 1 || self.blobs.len() > 256 {
            return Err("Unsupported Mapper project format".into());
        }
        let mut total = 0u64;
        for blob in &self.blobs {
            if blob.bytes > BLOB_LIMIT || !hash_valid(&blob.sha256) {
                return Err("Invalid project payload receipt".into());
            }
            total = total
                .checked_add(blob.bytes)
                .filter(|&n| n <= TOTAL_LIMIT)
                .ok_or("Project payloads exceed the 4 GiB format limit")?;
        }
        Ok(total)
    }
}
fn transfer(
    reader: &mut dyn Read,
    mut writer: Option<&mut dyn Write>,
    buffer: &mut [u8],
    cancel: &impl Fn() -> bool,
) -> Result<Option<Blob>, String> {
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    loop {
        if cancel() {
            return Ok(None);
        }
        let count = reader.read(buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        bytes += count as u64;
        if bytes > BLOB_LIMIT {
            return Err("Project payload exceeds 512 MiB".into());
        }
        hash.update(&buffer[..count]);
        if let Some(output) = writer.as_mut() {
            output
                .write_all(&buffer[..count])
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(Some(Blob {
        bytes,
        sha256: format!("{:x}", hash.finalize()),
    }))
}

fn transfer_buffer(blobs: &[Blob]) -> Vec<u8> {
    let largest = blobs.iter().map(|blob| blob.bytes).max().unwrap_or(0);
    vec![0; largest.clamp(1, TRANSFER_BYTES) as usize]
}

pub fn write(
    destination: &Path,
    recipe: Project,
    workspace: impl Serialize,
    sources: &[Source],
    cancel: impl Fn() -> bool,
) -> Result<bool, String> {
    write_with_publish_gate(destination, recipe, workspace, sources, &cancel, || !cancel())
}

/// Calls `begin_publish` after the temporary file is flushed, immediately
/// before it replaces the destination. A declined publication leaves the
/// destination untouched and returns `Ok(false)`.
pub fn write_with_publish_gate(
    destination: &Path,
    recipe: Project,
    workspace: impl Serialize,
    sources: &[Source],
    cancel: impl Fn() -> bool,
    begin_publish: impl FnOnce() -> bool,
) -> Result<bool, String> {
    if sources.len() > 256 {
        return Err("Too many project payloads".into());
    }
    let mut manifest = Manifest {
        format: "mapper-project".into(),
        schema: 1,
        recipe,
        workspace,
        blobs: Vec::new(),
    };
    let mut stamps = Vec::with_capacity(sources.len());
    for source in sources {
        if cancel() {
            return Ok(false);
        }
        let stamp = source.stamp()?;
        manifest.blobs.push(Blob {
            bytes: stamp.bytes,
            sha256: "0".repeat(64),
        });
        stamps.push(stamp);
    }
    manifest.validate()?;
    let pending = serde_json::to_vec(&manifest.blobs).map_err(|e| e.to_string())?;
    if cancel() {
        return Ok(false);
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut output = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    output
        .seek(SeekFrom::Start(HEADER_BYTES))
        .map_err(|e| e.to_string())?;
    let Some((prefix_bytes, mut metadata_hash)) =
        write_manifest_prefix(&mut output, &manifest, &cancel)?
    else {
        return Ok(false);
    };
    // Add the receipt field after the streamed object prefix. Its fixed-width
    // digests let us reserve the payload offset before hashing the blobs.
    const RECEIPT_FIELD: &[u8] = b",\"blobs\":";
    let receipt_start = prefix_bytes + RECEIPT_FIELD.len() as u64;
    let metadata_bytes = receipt_start + pending.len() as u64 + 1;
    if metadata_bytes > MANIFEST_LIMIT {
        return Err("Project metadata exceeds 64 MiB".into());
    }
    output
        .seek(SeekFrom::Start(HEADER_BYTES + prefix_bytes))
        .map_err(|e| e.to_string())?;
    output.write_all(RECEIPT_FIELD).map_err(|e| e.to_string())?;
    metadata_hash.update(RECEIPT_FIELD);
    output
        .seek(SeekFrom::Start(HEADER_BYTES + metadata_bytes))
        .map_err(|error| error.to_string())?;
    const CHANGED: &str = "A retained map changed while saving the project";
    let mut buffer = transfer_buffer(&manifest.blobs);
    for ((source, stamp), receipt) in sources.iter().zip(stamps).zip(&mut manifest.blobs) {
        let actual = match source {
            Source::File(path) | Source::Retained(RetainedFile { path, .. }) => {
                let mut file = File::open(path.as_ref()).map_err(|error| error.to_string())?;
                if SourceStamp::file(file.metadata())? != stamp {
                    return Err(CHANGED.into());
                }
                let actual = transfer(
                    &mut (&mut file).take(stamp.bytes),
                    Some(&mut output),
                    &mut buffer,
                    &cancel,
                )?;
                if actual.is_some() && SourceStamp::file(file.metadata())? != stamp {
                    return Err(CHANGED.into());
                }
                actual
            }
            Source::Bytes(bytes) => {
                transfer(&mut bytes.as_ref(), Some(&mut output), &mut buffer, &cancel)?
            }
        };
        let Some(actual) = actual else {
            return Ok(false);
        };
        if actual.bytes != receipt.bytes {
            return Err(CHANGED.into());
        }
        *receipt = actual;
    }
    let receipts = serde_json::to_vec(&manifest.blobs).map_err(|e| e.to_string())?;
    if receipt_start + receipts.len() as u64 + 1 != metadata_bytes {
        return Err("Project metadata length changed while saving".into());
    }
    output
        .seek(SeekFrom::Start(HEADER_BYTES + receipt_start))
        .map_err(|e| e.to_string())?;
    output.write_all(&receipts).map_err(|e| e.to_string())?;
    output.write_all(b"}").map_err(|e| e.to_string())?;
    metadata_hash.update(&receipts);
    metadata_hash.update(b"}");
    output.rewind().map_err(|error| error.to_string())?;
    write_header_fields(
        &mut output,
        metadata_bytes,
        &metadata_hash.finalize(),
    )?;
    if cancel() {
        return Ok(false);
    }
    output.as_file().sync_all().map_err(|e| e.to_string())?;
    if !begin_publish() {
        return Ok(false);
    }
    output
        .persist(destination)
        .map_err(|e| e.error.to_string())?;
    Ok(true)
}

#[derive(Serialize)]
struct ManifestPrefix<'a, W> {
    format: &'a str,
    schema: u32,
    recipe: &'a Project,
    workspace: &'a W,
}

/// Hash buffered JSON while holding back its last byte from the digest. The
/// caller replaces the closing brace with the receipt field after serialization.
struct ManifestStream<'a, C> {
    output: &'a mut dyn Write,
    cancel: &'a C,
    cancelled: bool,
    bytes: u64,
    hash: Sha256,
    last: Option<u8>,
}

impl<C: Fn() -> bool> Write for ManifestStream<'_, C> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if let Some((&last, prefix)) = input.split_last() {
            if (self.cancel)() {
                self.cancelled = true;
                return Err(io::Error::other("Project save cancelled"));
            }
            if self.bytes + input.len() as u64 > MANIFEST_LIMIT {
                return Err(io::Error::other("Project metadata exceeds 64 MiB"));
            }
            self.output.write_all(input)?;
            self.bytes += input.len() as u64;
            if let Some(previous) = self.last.replace(last) {
                self.hash.update([previous]);
            }
            self.hash.update(prefix);
        }
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

fn write_manifest_prefix(
    output: &mut impl Write,
    manifest: &Manifest<impl Serialize>,
    cancel: &impl Fn() -> bool,
) -> Result<Option<(u64, Sha256)>, String> {
    let mut stream = ManifestStream {
        output,
        cancel,
        cancelled: false,
        bytes: 0,
        hash: Sha256::new(),
        last: None,
    };
    let result = {
        let mut buffered = BufWriter::with_capacity(64 * 1024, &mut stream);
        serde_json::to_writer(
            &mut buffered,
            &ManifestPrefix {
                format: &manifest.format,
                schema: manifest.schema,
                recipe: &manifest.recipe,
                workspace: &manifest.workspace,
            },
        )
        .map_err(|e| e.to_string())
        .and_then(|()| buffered.flush().map_err(|e| e.to_string()))
    };
    if stream.cancelled {
        return Ok(None);
    }
    result?;
    if stream.last != Some(b'}') {
        return Err("Missing project metadata terminator".into());
    }
    Ok(Some((stream.bytes - 1, stream.hash)))
}

fn encode_manifest(manifest: &Manifest<impl Serialize>) -> Result<Vec<u8>, String> {
    let metadata = serde_json::to_vec(manifest).map_err(|e| e.to_string())?;
    if metadata.len() as u64 > MANIFEST_LIMIT {
        return Err("Project metadata exceeds 64 MiB".into());
    }
    Ok(metadata)
}
fn write_header(writer: &mut impl Write, metadata: &[u8]) -> Result<(), String> {
    write_header_fields(writer, metadata.len() as u64, &Sha256::digest(metadata))?;
    writer.write_all(metadata).map_err(|e| e.to_string())
}

fn write_header_fields(writer: &mut impl Write, bytes: u64, digest: &[u8]) -> Result<(), String> {
    writer.write_all(MAGIC).map_err(|e| e.to_string())?;
    writer
        .write_all(&bytes.to_le_bytes())
        .map_err(|e| e.to_string())?;
    writer.write_all(digest).map_err(|e| e.to_string())
}
/// Update a bundled project's recipe while retaining every frozen map and receipt.
/// Returns false for legacy JSON input so callers can retain their JSON writer.
pub fn rewrite_recipe(
    source: &Path,
    destination: &Path,
    recipe: Project,
    overwrite: bool,
    cancel: impl Fn() -> bool,
) -> Result<bool, String> {
    rewrite_recipe_checked(source, destination, recipe, overwrite, cancel, None, || {
        Ok(())
    })
}

fn rewrite_recipe_checked(
    source: &Path,
    destination: &Path,
    recipe: Project,
    overwrite: bool,
    cancel: impl Fn() -> bool,
    expected_header: Option<&[u8; 40]>,
    check: impl FnOnce() -> Result<(), String>,
) -> Result<bool, String> {
    let mut input = File::open(source).map_err(|e| e.to_string())?;
    let Some(BundleMetadata { content: metadata, header }) = read_metadata(&mut input)? else {
        return Ok(false);
    };
    decode_manifest::<CheckedWorkspace>(&input, &metadata)?;
    if expected_header.is_some_and(|expected| *expected != header) {
        return Err(EDIT_SOURCE_CHANGED.into());
    }
    // The checked source workspace is opaque to a recipe edit. Borrow its
    // original encoding until it has been written to the destination header.
    let mut manifest: Manifest<&RawValue> =
        serde_json::from_slice(&metadata).map_err(|e| e.to_string())?;
    manifest.recipe = recipe;
    manifest.validate()?;
    if cancel() {
        return Err("Project update cancelled".into());
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut output = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    write_header(&mut output, &encode_manifest(&manifest)?)?;
    let receipts = manifest.blobs;
    drop(metadata);
    let mut buffer = transfer_buffer(&receipts);
    for receipt in &receipts {
        let actual = transfer(
            &mut (&mut input).take(receipt.bytes),
            Some(&mut output),
            &mut buffer,
            &cancel,
        )?
        .ok_or("Project update cancelled")?;
        if actual.bytes != receipt.bytes || actual.sha256 != receipt.sha256 {
            return Err("Saved map payload failed its project checksum".into());
        }
    }
    drop(input);
    if cancel() {
        return Err("Project update cancelled".into());
    }
    output.as_file().sync_all().map_err(|e| e.to_string())?;
    if cancel() {
        return Err("Project update cancelled".into());
    }
    check()?;
    if overwrite {
        output
            .persist(destination)
            .map_err(|e| e.error.to_string())?;
    } else {
        output
            .persist_noclobber(destination)
            .map_err(|e| e.error.to_string())?;
    }
    Ok(true)
}

/// A saved recipe and its project file revision for an operation that can
/// finish after another editor has saved the project.
pub struct RecipeEdit<'a> {
    path: &'a Path,
    stamp: SourceStamp,
    identity: RecipeIdentity,
    pub recipe: Project,
}

enum RecipeIdentity {
    Bundle([u8; 40]),
    Json([u8; 32]),
}

pub(crate) fn read_recipe_json(file: &mut File) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    file.take(MAX_PROJECT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_PROJECT_BYTES {
        return Err("Project exceeds the 4 MiB limit.".into());
    }
    Ok(bytes)
}

impl<'a> RecipeEdit<'a> {
    pub fn open(path: &'a Path) -> Result<Self, String> {
        let mut file = File::open(path).map_err(|error| error.to_string())?;
        let stamp = SourceStamp::file(file.metadata())?;
        let (recipe, identity) = match read_manifest::<CheckedWorkspace>(&mut file)? {
            Some(BundleMetadata { content: manifest, header }) => {
                (manifest.recipe, RecipeIdentity::Bundle(header))
            },
            None => {
                let raw = read_recipe_json(&mut file)?;
                (
                    Project::decode_json(&raw)?,
                    RecipeIdentity::Json(Sha256::digest(raw).into()),
                )
            }
        };
        if SourceStamp::file(std::fs::metadata(path))? != stamp {
            return Err(EDIT_SOURCE_CHANGED.into());
        }
        Ok(Self {
            path,
            stamp,
            recipe,
            identity,
        })
    }

    pub fn ensure_current(&self) -> Result<(), String> {
        let mut file = File::open(self.path).map_err(|_| EDIT_SOURCE_CHANGED)?;
        if SourceStamp::file(file.metadata())? != self.stamp {
            return Err(EDIT_SOURCE_CHANGED.into());
        }
        let original = match &self.identity {
            RecipeIdentity::Bundle(expected) => {
                let mut header = [0; HEADER_BYTES as usize];
                file.read_exact(&mut header).is_ok()
                    && &header[..8] == MAGIC
                    && &header[8..] == expected
            }
            RecipeIdentity::Json(expected) => {
                Sha256::digest(read_recipe_json(&mut file)?).as_slice() == expected
            }
        };
        if !original || SourceStamp::file(std::fs::metadata(self.path))? != self.stamp {
            return Err(EDIT_SOURCE_CHANGED.into());
        }
        Ok(())
    }

    pub fn save(
        &self,
        destination: &Path,
        recipe: Project,
        overwrite: bool,
        cancel: impl Fn() -> bool,
    ) -> Result<(), String> {
        recipe.validate()?;
        let check = || {
            if cancel() {
                return Err("Project update cancelled".into());
            }
            self.ensure_current()?;
            if cancel() {
                return Err("Project update cancelled".into());
            }
            Ok(())
        };
        check()?;
        let parent = destination
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        match &self.identity {
            RecipeIdentity::Bundle(header) => {
                if !rewrite_recipe_checked(
                    self.path,
                    destination,
                    recipe,
                    overwrite,
                    &cancel,
                    Some(header),
                    check,
                )? {
                    return Err(EDIT_SOURCE_CHANGED.into());
                }
            }
            RecipeIdentity::Json(_) => {
                crate::profile::write_document_checked(destination, &recipe, overwrite, check)?;
            }
        }
        Ok(())
    }
}
// Walk the ordinary JSON parser to retain string, number and nesting checks
// when the caller only needs the recipe and payload receipts.
struct CheckedWorkspace;

impl<'de> Deserialize<'de> for CheckedWorkspace {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(Self)
    }
}

impl<'de> de::Visitor<'de> for CheckedWorkspace {
    type Value = Self;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("valid workspace JSON")
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self, E> {
        Ok(self)
    }

    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self, E> {
        Ok(self)
    }

    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self, E> {
        Ok(self)
    }

    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self, E> {
        Ok(self)
    }

    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self, E> {
        Ok(self)
    }

    fn visit_str<E: de::Error>(self, _: &str) -> Result<Self, E> {
        Ok(self)
    }

    fn visit_seq<A: de::SeqAccess<'de>>(self, mut sequence: A) -> Result<Self, A::Error> {
        while sequence.next_element::<Self>()?.is_some() {}
        Ok(self)
    }

    fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<Self, A::Error> {
        while map.next_entry::<Self, Self>()?.is_some() {}
        Ok(self)
    }
}

struct BundleMetadata<T> {
    content: T,
    header: [u8; 40],
}

fn read_metadata_header(file: &mut File) -> Result<Option<([u8; 40], u64)>, String> {
    let mut magic = [0; 8];
    let count = file.read(&mut magic).map_err(|e| e.to_string())?;
    if count != 8 || &magic != MAGIC {
        file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        return Ok(None);
    }
    let mut header = [0; 40];
    file.read_exact(&mut header).map_err(|e| e.to_string())?;
    let length = u64::from_le_bytes(header[..8].try_into().unwrap());
    if length > MANIFEST_LIMIT {
        return Err("Project metadata exceeds 64 MiB".into());
    }
    Ok(Some((header, length)))
}

fn read_metadata(file: &mut File) -> Result<Option<BundleMetadata<Vec<u8>>>, String> {
    let Some((header, length)) = read_metadata_header(file)? else {
        return Ok(None);
    };
    let content = read_metadata_content(file, &header, length)?;
    Ok(Some(BundleMetadata { content, header }))
}

fn read_metadata_content(file: &mut File, header: &[u8; 40], length: u64) -> Result<Vec<u8>, String> {
    let mut metadata = vec![0; length as usize];
    file.read_exact(&mut metadata).map_err(|e| e.to_string())?;
    if Sha256::digest(&metadata).as_slice() != &header[8..] {
        return Err("Project metadata checksum differs".into());
    }
    Ok(metadata)
}

fn decode_manifest<'de, W: Deserialize<'de>>(
    file: &File,
    metadata: &'de [u8],
) -> Result<Manifest<W>, String> {
    let manifest: Manifest<W> = serde_json::from_slice(metadata).map_err(|e| e.to_string())?;
    check_manifest_length(file, metadata.len() as u64, &manifest)?;
    Ok(manifest)
}

fn check_manifest_length<W>(file: &File, length: u64, manifest: &Manifest<W>) -> Result<(), String> {
    let payload_bytes = manifest.validate()?;
    if file.metadata().map_err(|e| e.to_string())?.len()
        != HEADER_BYTES + length + payload_bytes
    {
        return Err("Project length differs from its payload receipts".into());
    }
    Ok(())
}

struct MetadataReader<'a, C> {
    input: io::Take<&'a mut File>,
    hash: Sha256,
    cancel: &'a C,
    cancelled: bool,
}

impl<C: Fn() -> bool> Read for MetadataReader<'_, C> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.input.limit() == 0 || buffer.is_empty() {
            return Ok(0);
        }
        if self.cancelled || (self.cancel)() {
            self.cancelled = true;
            return Err(io::Error::other("Project open cancelled"));
        }
        let count = self.input.read(buffer)?;
        self.hash.update(&buffer[..count]);
        Ok(count)
    }
}

/// The bounded reader hashes whole buffers even though the JSON decoder reads
/// individual bytes. Validate the complete metadata before exposing a manifest.
fn read_verified_manifest<W: de::DeserializeOwned>(
    file: &mut File,
    header: &[u8; 40],
    length: u64,
    cancel: &impl Fn() -> bool,
) -> Result<Option<Manifest<W>>, String> {
    let mut stream = MetadataReader {
        input: (&mut *file).take(length),
        hash: Sha256::new(),
        cancel,
        cancelled: false,
    };
    let (parsed, drained) = {
        let mut reader = BufReader::with_capacity(64 * 1024, &mut stream);
        let parsed = serde_json::from_reader(&mut reader);
        // Invalid JSON can stop before the end of metadata. Finish checking
        // its digest so corrupted bundles still report a checksum failure.
        let drained = io::copy(&mut reader, &mut io::sink());
        (parsed, drained)
    };
    if stream.cancelled {
        return Ok(None);
    }
    drained.map_err(|e| e.to_string())?;
    if stream.input.limit() != 0 {
        return Err("Project metadata is truncated".into());
    }
    if stream.hash.finalize().as_slice() != &header[8..] {
        return Err("Project metadata checksum differs".into());
    }
    let manifest = parsed.map_err(|e| e.to_string())?;
    check_manifest_length(file, length, &manifest)?;
    Ok(Some(manifest))
}

fn read_manifest<W: de::DeserializeOwned>(
    file: &mut File,
) -> Result<Option<BundleMetadata<Manifest<W>>>, String> {
    let Some(BundleMetadata { content: metadata, header }) = read_metadata(file)? else {
        return Ok(None);
    };
    Ok(Some(BundleMetadata {
        content: decode_manifest(file, &metadata)?,
        header,
    }))
}
/// CLI callers can read the generation recipe without unpacking world payloads.
pub fn recipe(path: &Path) -> Result<Option<Project>, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    Ok(read_manifest::<CheckedWorkspace>(&mut file)?.map(|metadata| metadata.content.recipe))
}

pub(crate) fn load_recipe(path: &Path) -> Result<Project, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    if let Some(metadata) = read_manifest::<CheckedWorkspace>(&mut file)? {
        return Ok(metadata.content.recipe);
    }
    Project::decode_json(&read_recipe_json(&mut file)?)
}

/// Check payload integrity without extracting maps to temporary files.
pub fn verify(path: &Path, cancel: impl Fn() -> bool) -> Result<Option<usize>, String> {
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let Some(metadata) = read_manifest::<CheckedWorkspace>(&mut file)? else {
        return Ok(None);
    };
    let manifest = metadata.content;
    let mut buffer = transfer_buffer(&manifest.blobs);
    for receipt in &manifest.blobs {
        let actual = transfer(
            &mut (&mut file).take(receipt.bytes),
            None,
            &mut buffer,
            &cancel,
        )?
        .ok_or("Project verification cancelled")?;
        if actual.bytes != receipt.bytes || actual.sha256 != receipt.sha256 {
            return Err("Saved map payload failed its project checksum".into());
        }
    }
    Ok(Some(manifest.blobs.len()))
}
pub fn load(path: &Path, cancel: impl Fn() -> bool) -> Result<Option<Loaded>, String> {
    load_with_workspace(path, cancel)
}

/// Decode the workspace directly into the caller's document type.
/// The default represents a legacy recipe without a workspace.
pub fn load_with_workspace<W: de::DeserializeOwned + Default>(
    path: &Path,
    cancel: impl Fn() -> bool,
) -> Result<Option<Loaded<W>>, String> {
    load_bundle(path, &cancel, |file, blobs, _| {
        all_payloads(load_serial(file, blobs, &|_| true, &cancel)?)
    })
}

/// Restore a desktop workspace using idle workers for independent payloads.
pub fn load_workspace_parallel<W: de::DeserializeOwned + Default>(
    path: &Path,
    cancel: impl Fn() -> bool + Sync,
) -> Result<Option<Loaded<W>>, String> {
    load_bundle(path, &cancel, |file, blobs, _| {
        all_payloads(read_workspace_payloads(path, file, blobs, &|_| true, &cancel)?)
    })
}

type Retained = Vec<Option<Arc<tempfile::TempPath>>>;

/// Verify every stored span but extract only payloads referenced by the
/// workspace. Selection is resolved from its verified manifest before I/O.
pub fn load_workspace_selected<W: de::DeserializeOwned + Default>(
    path: &Path,
    select: impl FnOnce(&W, usize) -> Result<Vec<bool>, String>,
    cancel: impl Fn() -> bool + Sync,
) -> Result<Option<Loaded<W, Option<Arc<tempfile::TempPath>>>>, String> {
    load_bundle(path, &cancel, |file, blobs, workspace| {
        let selected = select(workspace, blobs.len())?;
        if selected.len() != blobs.len() {
            return Err("Workspace payload selection differs from its receipts".into());
        }
        read_workspace_payloads(path, file, blobs, &|index| selected[index], &cancel)
    })
}

fn all_payloads(retained: Option<Retained>) -> Result<Option<Vec<Arc<tempfile::TempPath>>>, String> {
    retained.map(|blobs| {
        blobs.into_iter().collect::<Option<Vec<_>>>()
            .ok_or_else(|| "Missing verified project payload".into())
    }).transpose()
}

fn read_workspace_payloads(
    path: &Path,
    file: &mut File,
    blobs: &[Blob],
    selected: &(impl Fn(usize) -> bool + Sync),
    cancel: &(impl Fn() -> bool + Sync),
) -> Result<Option<Retained>, String> {
    if let Some(result) = load_parallel(path, file, blobs, selected, cancel)? {
        return Ok(result);
    }
    load_serial(file, blobs, selected, cancel)
}

fn load_bundle<W: de::DeserializeOwned + Default, Payload>(
    path: &Path,
    cancel: &impl Fn() -> bool,
    read: impl FnOnce(&mut File, &[Blob], &W) -> Result<Option<Vec<Payload>>, String>,
) -> Result<Option<Loaded<W, Payload>>, String> {
    if cancel() {
        return Ok(None);
    }
    let mut file = File::open(path).map_err(|e| e.to_string())?;
    let Some((header, length)) = read_metadata_header(&mut file)? else {
        let recipe = Project::decode_json(&read_recipe_json(&mut file)?)?;
        if cancel() {
            return Ok(None);
        }
        return Ok(Some(Loaded {
            manifest: Manifest {
                format: "mapper-project".into(),
                schema: 1,
                recipe,
                workspace: W::default(),
                blobs: Vec::new(),
            },
            blobs: Vec::new(),
        }));
    };
    let manifest = if length <= IN_MEMORY_MANIFEST_LIMIT {
        // Keep the slice parser for ordinary projects, where the input buffer
        // is small and faster JSON decoding matters more than peak memory.
        let metadata = read_metadata_content(&mut file, &header, length)?;
        decode_manifest(&file, &metadata)?
    } else {
        let Some(manifest) = read_verified_manifest(&mut file, &header, length, cancel)? else {
            return Ok(None);
        };
        manifest
    };
    if cancel() {
        return Ok(None);
    }
    let Some(blobs) = read(&mut file, &manifest.blobs, &manifest.workspace)? else {
        return Ok(None);
    };
    Ok((!cancel()).then_some(Loaded { manifest, blobs }))
}

fn read_payload(
    file: &mut File,
    receipt: &Blob,
    output: Option<&mut dyn Write>,
    buffer: &mut [u8],
    cancel: &impl Fn() -> bool,
) -> Result<bool, String> {
    let Some(actual) = transfer(&mut file.take(receipt.bytes), output, buffer, cancel)? else {
        return Ok(false);
    };
    if actual.bytes != receipt.bytes || actual.sha256 != receipt.sha256 {
        return Err("A saved map payload failed its project checksum".into());
    }
    Ok(true)
}

fn load_serial(
    file: &mut File,
    receipts: &[Blob],
    selected: &impl Fn(usize) -> bool,
    cancel: &impl Fn() -> bool,
) -> Result<Option<Retained>, String> {
    let mut blobs = Vec::with_capacity(receipts.len());
    let mut retained = HashMap::<_, Arc<tempfile::TempPath>>::new();
    let mut buffer = transfer_buffer(receipts);
    for (index, receipt) in receipts.iter().enumerate() {
        if !selected(index) {
            if !read_payload(file, receipt, None, &mut buffer, cancel)? {
                return Ok(None);
            }
            blobs.push(None);
            continue;
        }
        let key = (receipt.bytes, receipt.sha256.as_str());
        let next = retained.entry(key);
        let mut output = if matches!(next, std::collections::hash_map::Entry::Vacant(_)) {
            Some(tempfile::NamedTempFile::new().map_err(|e| e.to_string())?)
        } else {
            None
        };
        // Every on-disk span must match its receipt, including duplicates.
        if !read_payload(
            file,
            receipt,
            output.as_mut().map(|file| file as &mut dyn Write),
            &mut buffer,
            cancel,
        )? {
            return Ok(None);
        }
        let payload = next.or_insert_with(|| {
            Arc::new(
                output
                    .expect("New payload receipts have a temporary file")
                    .into_temp_path(),
            )
        });
        blobs.push(Some(payload.clone()));
    }
    Ok(Some(blobs))
}

#[cfg(windows)]
fn payload_file_identity(file: &File) -> Option<papyrus_file_id::Identity> {
    papyrus_file_id::identity(file).ok()
}

#[cfg(unix)]
fn payload_file_identity(file: &File) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata().ok()?;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(any(unix, windows)))]
fn payload_file_identity(_: &File) -> Option<()> {
    None
}

// The outer option allows serial fallback, including when the source path
// points at another file by the time the extra read handles are opened.
fn load_parallel(
    path: &Path,
    file: &mut File,
    receipts: &[Blob],
    selected: &(impl Fn(usize) -> bool + Sync),
    cancel: &(impl Fn() -> bool + Sync),
) -> Result<Option<Option<Retained>>, String> {
    use crate::generation_resources::{current, try_with_cpu_pool};

    let width = current().effective_workers().min(4).min(receipts.len());
    if width < 2
        || receipts.iter().filter(|blob| blob.bytes >= 1024 * 1024).take(2).count() < 2
    {
        return Ok(None);
    }
    try_with_cpu_pool(|pool| {
        if cancel() {
            return Ok(Some(None));
        }
        let width = width.min(pool.current_num_threads());
        if width < 2 {
            return Ok(None);
        }
        let Some(identity) = payload_file_identity(file) else {
            return Ok(None);
        };
        // Only open extra source handles after acquiring idle workers. When
        // generation is busy, serial restore can start on the original handle.
        let Some(mut readers) = (0..width)
            .map(|_| {
                let reader = File::open(path).ok()?;
                (payload_file_identity(&reader) == Some(identity))
                    .then(|| (reader, transfer_buffer(receipts)))
            })
            .collect::<Option<Vec<_>>>()
        else {
            return Ok(None);
        };
        let mut offset = file.stream_position().map_err(|error| error.to_string())?;
        let mut seen = std::collections::HashSet::new();
        let spans: Vec<_> = receipts
            .iter()
            .enumerate()
            .map(|(index, receipt)| {
                let copy = selected(index) && seen.insert((receipt.bytes, receipt.sha256.as_str()));
                let span = (offset, receipt, copy);
                offset += receipt.bytes;
                span
            })
            .collect();
        let next = AtomicUsize::new(0);
        let first_failure = AtomicUsize::new(spans.len());
        let mut checks = pool.install(|| {
            readers
                .par_iter_mut()
                .flat_map_iter(|(file, buffer)| {
                    std::iter::from_fn(|| {
                        // Readers claim successive spans as they finish. A
                        // small preview can finish while another reader is
                        // still hashing a large map, without a batch barrier.
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        if index >= first_failure.load(Ordering::Relaxed) {
                            return None;
                        }
                        let (offset, receipt, copy) = spans[index];
                        let check = (|| {
                            if cancel() {
                                return Ok(None);
                            }
                            file.seek(SeekFrom::Start(offset)).map_err(|error| error.to_string())?;
                            let mut output = copy.then(tempfile::NamedTempFile::new).transpose()
                                .map_err(|error| error.to_string())?;
                            let writer = output.as_mut().map(|file| file as &mut dyn Write);
                            if !read_payload(file, receipt, writer, buffer, cancel)? {
                                return Ok(None);
                            }
                            Ok::<_, String>(Some(output.map(|file| Arc::new(file.into_temp_path()))))
                        })();
                        if !matches!(check, Ok(Some(_))) {
                            first_failure.fetch_min(index, Ordering::Relaxed);
                        }
                        Some((index, check))
                    })
                })
                .collect::<Vec<_>>()
        });
        // Preserve the manifest order for both errors and shared payloads.
        checks.sort_unstable_by_key(|(index, _)| *index);
        let mut retained = HashMap::new();
        let mut blobs = Vec::with_capacity(receipts.len());
        for (index, check) in checks {
            let Some(payload) = check? else {
                return Ok(Some(None));
            };
            if !selected(index) {
                blobs.push(None);
                continue;
            }
            let receipt = &receipts[index];
            let key = (receipt.bytes, receipt.sha256.as_str());
            if let Some(payload) = payload {
                retained.insert(key, payload);
            }
            let file = retained.get(&key).ok_or("Missing verified project payload")?;
            blobs.push(Some(Arc::clone(file)));
        }
        if blobs.len() != receipts.len() {
            return Err("Missing verified project payload".into());
        }
        Ok(Some(Some(blobs)))
    })
    .map(Option::flatten)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sources() -> Vec<Source> {
        vec![
            Source::Bytes(Arc::from(b"first retained map".as_slice())),
            Source::Bytes(Arc::from(b"render layers".as_slice())),
        ]
    }
    #[test]
    fn portable_payloads_roundtrip_and_cli_reads_the_same_recipe() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("world.mapper");
        let recipe = Project::default();
        let metadata = serde_json::json!({
            "schema": 1,
            "blobs": [{"bytes": 18, "sha256": "0".repeat(64)}],
            "note": "map workspace",
        });
        assert!(
            write(&path, recipe.clone(), metadata.clone(), &sources(), || {
                false
            })
            .unwrap()
        );
        assert_eq!(Project::load(&path).unwrap(), recipe);
        assert_eq!(verify(&path, || false).unwrap(), Some(2));
        let loaded = load(&path, || false).unwrap().unwrap();
        assert_eq!(loaded.manifest.workspace, metadata);
        let encoded = encode_manifest(&loaded.manifest).unwrap();
        let file = std::fs::read(&path).unwrap();
        let metadata_end = HEADER_BYTES as usize + encoded.len();
        assert_eq!(&file[HEADER_BYTES as usize..metadata_end], encoded);
        assert_eq!(
            std::fs::read(loaded.blobs[0].as_ref()).unwrap(),
            b"first retained map"
        );
        assert_eq!(
            std::fs::read(loaded.blobs[1].as_ref()).unwrap(),
            b"render layers"
        );
        let retained = loaded.blobs[0].to_path_buf();
        drop(loaded);
        assert!(!retained.exists());
    }

    #[test]
    fn typed_metadata_streams_across_chunks_and_cancels_before_publication() {
        #[derive(Serialize)]
        struct Workspace {
            labels: Vec<String>,
        }
        let workspace = Workspace {
            labels: vec!["Slope \"°\"".repeat(512); 64],
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("world.mapper");
        let sources = sources();
        assert!(write(&path, Project::default(), &workspace, &sources, || false).unwrap());
        let loaded = load(&path, || false).unwrap().unwrap();
        assert_eq!(
            loaded.manifest.workspace,
            serde_json::to_value(&workspace).unwrap()
        );
        assert_eq!(verify(&path, || false).unwrap(), Some(sources.len()));
        let before = std::fs::read(&path).unwrap();
        let checks = std::cell::Cell::new(0);
        assert!(
            !write(&path, Project::default(), &workspace, &[], || {
                checks.set(checks.get() + 1);
                checks.get() == 3
            })
            .unwrap()
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn large_workspace_open_is_cancellable_and_keeps_payload_boundaries() {
        #[derive(Default, Serialize, Deserialize)]
        struct Workspace {
            label: String,
        }
        let workspace = Workspace {
            label: "é".repeat(IN_MEMORY_MANIFEST_LIMIT as usize / 2),
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.mapper");
        assert!(write(&path, Project::default(), &workspace, &sources(), || false).unwrap());

        let checks = std::cell::Cell::new(0);
        assert!(
            load_with_workspace::<Workspace>(&path, || {
                checks.set(checks.get() + 1);
                checks.get() >= 3
            })
            .unwrap()
            .is_none()
        );

        let loaded = load_with_workspace::<Workspace>(&path, || false)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.manifest.workspace.label, workspace.label);
        assert_eq!(std::fs::read(loaded.blobs[0].as_ref()).unwrap(), b"first retained map");
        assert_eq!(std::fs::read(loaded.blobs[1].as_ref()).unwrap(), b"render layers");

        let mut source = File::options().write(true).open(&path).unwrap();
        source.seek(SeekFrom::Start(HEADER_BYTES)).unwrap();
        source.write_all(b"!").unwrap();
        drop(source);
        assert_eq!(
            load_with_workspace::<Workspace>(&path, || false).err().unwrap(),
            "Project metadata checksum differs",
        );
    }

    #[test]
    fn equal_payloads_share_storage_and_every_copy_is_checked() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("duplicate.mapper");
        let encoded_map: Arc<[u8]> = vec![42; 300 * 1024].into();
        let sources = [
            Source::Bytes(encoded_map.clone()),
            Source::Bytes(Arc::from(b"map overview".as_slice())),
            Source::Bytes(encoded_map.clone()),
        ];
        assert!(
            write(
                &path,
                Project::default(),
                serde_json::Value::Null,
                &sources,
                || false
            )
            .unwrap()
        );
        let loaded = load(&path, || false).unwrap().unwrap();
        assert_eq!(loaded.blobs.len(), sources.len());
        assert!(Arc::ptr_eq(&loaded.blobs[0], &loaded.blobs[2]));
        assert!(!Arc::ptr_eq(&loaded.blobs[0], &loaded.blobs[1]));
        assert_eq!(
            std::fs::read(loaded.blobs[0].as_ref()).unwrap(),
            *encoded_map
        );
        let old = loaded.blobs[2].clone();
        drop(loaded);

        let mut file = File::options().write(true).open(&path).unwrap();
        file.seek(SeekFrom::End(-1)).unwrap();
        file.write_all(&[7]).unwrap();
        drop(file);
        assert!(verify(&path, || false).is_err());
        assert!(load(&path, || false).is_err());
        assert_eq!(std::fs::read(old.as_ref()).unwrap(), *encoded_map);
    }
    #[test]
    fn legacy_json_projects_remain_readable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy.mapper");
        let recipe = Project::default();
        recipe.save(&path).unwrap();
        assert!(self::recipe(&path).unwrap().is_none());
        let loaded = load(&path, || false).unwrap().unwrap();
        assert_eq!(loaded.manifest.recipe, recipe);
        assert!(loaded.blobs.is_empty());
        assert!(loaded.manifest.workspace.is_null());
    }

    #[test]
    fn recipe_only_reads_preserve_workspace_validation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("world.mapper");
        let output = directory.path().join("edited.mapper");
        let recipe = Project::default();
        let manifest = Manifest {
            format: "mapper-project".into(),
            schema: 1,
            recipe: recipe.clone(),
            workspace: serde_json::Value::Null,
            blobs: Vec::new(),
        };
        let template = String::from_utf8(encode_manifest(&manifest).unwrap()).unwrap();
        let nested = "[".repeat(140) + "0" + &"]".repeat(140);
        for workspace in ["1e999", r#"{"\ud800":0}"#, "{\"entry\":}", &nested] {
            let json =
                template.replace("\"workspace\":null", &format!("\"workspace\":{workspace}"));
            write_header(&mut File::create(&path).unwrap(), json.as_bytes()).unwrap();
            assert!(Project::load(&path).is_err(), "{workspace}");
            assert!(verify(&path, || false).is_err(), "{workspace}");
            assert!(RecipeEdit::open(&path).is_err(), "{workspace}");
            assert!(load(&path, || false).is_err(), "{workspace}");
            assert!(rewrite_recipe(&path, &output, recipe.clone(), false, || false).is_err());
            assert!(!output.exists());
        }
        let missing = template.replace("\"workspace\":null,", "");
        assert_ne!(missing, template);
        write_header(&mut File::create(&path).unwrap(), missing.as_bytes()).unwrap();
        assert!(load_with_workspace::<Option<serde_json::Value>>(&path, || false).is_err());
        write_header(&mut File::create(&path).unwrap(), template.as_bytes()).unwrap();
        let loaded = load_with_workspace::<Option<serde_json::Value>>(&path, || false)
            .unwrap()
            .unwrap();
        assert!(loaded.manifest.workspace.is_none());
    }

    #[test]
    fn recipe_rewrite_copies_the_workspace_encoding() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("original.mapper");
        let output = directory.path().join("edited.mapper");
        let workspace = r#"{ "escaped\u0020key": [true, null, { "n": -0.25e2, "z": "\u00e9" }] }"#;
        let manifest = Manifest {
            format: "mapper-project".into(),
            schema: 1,
            recipe: Project::default(),
            workspace: serde_json::from_str::<&RawValue>(workspace).unwrap(),
            blobs: Vec::new(),
        };
        write_header(
            &mut File::create(&path).unwrap(),
            &encode_manifest(&manifest).unwrap(),
        )
        .unwrap();
        assert_eq!(verify(&path, || false).unwrap(), Some(0));
        let edit = RecipeEdit::open(&path).unwrap();
        let mut next = edit.recipe.clone();
        next.size = 3000;
        edit.save(&output, next.clone(), false, || false).unwrap();
        assert_eq!(Project::load(&output).unwrap(), next);
        let mut file = File::open(output).unwrap();
        let encoded = read_metadata(&mut file).unwrap().unwrap().content;
        let decoded = decode_manifest::<&RawValue>(&file, &encoded).unwrap();
        assert_eq!(decoded.workspace.get(), workspace);
    }
    #[test]
    fn recipe_updates_preserve_workspace_and_world_payloads_in_place_and_in_copies() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("world.mapper");
        let copy = directory.path().join("copy.mapper");
        let workspace = serde_json::json!({"frozen_recipe":{"seed":123},"maps":[1,2]});
        write(
            &path,
            Project::default(),
            workspace.clone(),
            &sources(),
            || false,
        )
        .unwrap();
        let next = Project {
            size: 2000,
            ..Default::default()
        };
        assert!(rewrite_recipe(&path, &copy, next.clone(), false, || false).unwrap());
        next.save(&path).unwrap();
        for updated in [&path, &copy] {
            let loaded = load(updated, || false).unwrap().unwrap();
            assert_eq!(loaded.manifest.recipe, next);
            assert_eq!(loaded.manifest.workspace, workspace);
            assert_eq!(
                std::fs::read(loaded.blobs[0].as_ref()).unwrap(),
                b"first retained map"
            );
        }
        let before = std::fs::read(&copy).unwrap();
        assert!(rewrite_recipe(&path, &copy, Project::default(), false, || false).is_err());
        assert!(rewrite_recipe(&path, &copy, Project::default(), true, || true).is_err());
        assert_eq!(std::fs::read(&copy).unwrap(), before);
    }
    #[test]
    fn corrupt_and_truncated_projects_fail_before_publishing_a_document() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("world.mapper");
        write(
            &path,
            Project::default(),
            serde_json::Value::Null,
            &sources(),
            || false,
        )
        .unwrap();
        let original = std::fs::read(&path).unwrap();
        for case in 0..5 {
            let mut bytes = original.clone();
            match case {
                0 => bytes[48] ^= 1,
                1 => *bytes.last_mut().unwrap() ^= 1,
                2 => {
                    bytes.pop();
                }
                3 => bytes.push(0),
                _ => bytes[8..16].copy_from_slice(&u64::MAX.to_le_bytes()),
            }
            std::fs::write(&path, bytes).unwrap();
            assert!(load(&path, || false).is_err(), "Case {case}");
            assert!(verify(&path, || false).is_err(), "Case {case}");
        }
    }
    #[test]
    fn cancellation_and_changed_sources_preserve_the_existing_project() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("world.mapper");
        std::fs::write(&path, b"previous project").unwrap();
        for stop in 0..8 {
            let calls = std::cell::Cell::new(0);
            let result = write(
                &path,
                Project::default(),
                serde_json::Value::Null,
                &sources(),
                || {
                    let call = calls.get();
                    calls.set(call + 1);
                    call >= stop
                },
            )
            .unwrap();
            if !result {
                assert_eq!(std::fs::read(&path).unwrap(), b"previous project");
            }
        }
        let source = tempfile::NamedTempFile::new_in(directory.path())
            .unwrap()
            .into_temp_path();
        std::fs::write(&source, b"original payload").unwrap();
        std::fs::write(&path, b"previous project").unwrap();
        let source = Arc::new(source);
        let calls = std::cell::Cell::new(0);
        let result = write(
            &path,
            Project::default(),
            serde_json::Value::Null,
            &[Source::File(source.clone())],
            || {
                calls.set(calls.get() + 1);
                if calls.get() == 4 {
                    std::fs::write(source.as_ref(), b"modified payload").unwrap();
                    // Keep the length fixed and make the write-time change
                    // deterministic even on a coarse-grained filesystem.
                    File::options()
                        .write(true)
                        .open(source.as_ref())
                        .unwrap()
                        .set_modified(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(60))
                        .unwrap();
                }
                false
            },
        );
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"previous project");
        assert!(load(&path, || true).unwrap().is_none());
    }
}
