//! Versioned updates for packaged Windows builds. The current installation is
//! never overwritten while it is running.
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Seek, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};

const CHANNEL_FILE: &str = "update-channel.json";
const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;
const MAX_EXPANDED: u64 = 2 * 1024 * 1024 * 1024;
const REQUIRED: &[&str] = &[
    "mapper-studio.exe",
    "mapper.exe",
    "release-manifest.json",
    "native/Papyrus.NativeWorker.exe",
    "tools/mapper-asset-reader.exe",
    "tools/mapper-texture-sources.exe",
];

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Channel {
    pub repository: String,
    pub version: String,
}

#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    url: String,
    size: u64,
    digest: String,
}

impl Channel {
    pub fn installed() -> Option<Self> {
        Self::read(std::env::current_exe().ok()?.parent()?).ok()
    }

    fn read(directory: &Path) -> Result<Self, String> {
        let channel: Self = serde_json::from_reader(
            File::open(directory.join(CHANNEL_FILE))
                .map_err(error)?
                .take(4096),
        )
        .map_err(error)?;
        if !cfg!(all(windows, target_arch = "x86_64"))
            || !valid_repo(&channel.repository)
            || Version::parse(&channel.version).is_err()
        {
            return Err("Invalid Mapper update channel".into());
        }
        Ok(channel)
    }

    fn store(&self) -> Result<PathBuf, String> {
        let settings = crate::preferences::Preferences::path()?;
        let parent = settings.parent().ok_or("Mapper data folder unavailable")?;
        let key = format!("{:x}", Sha256::digest(self.repository.as_bytes()));
        Ok(parent.join("updates").join(key))
    }

    pub fn check(&self, cancel: impl Fn() -> bool) -> Result<Option<Release>, String> {
        self.check_asset(ReleaseKind::Portable, None, 15, cancel)
    }

    pub fn newer_installer(&self, installed: Option<&str>) -> Result<Option<[String; 3]>, String> {
        let installed = installed.and_then(|value| Version::parse(value).ok());
        self.check_asset(ReleaseKind::Installer, installed.as_ref(), 5, || false)
            .map(|release| release.map(|release| [release.version, release.url, release.digest]))
    }

    fn check_asset(
        &self,
        kind: ReleaseKind,
        installed: Option<&Version>,
        timeout: u32,
        cancel: impl Fn() -> bool,
    ) -> Result<Option<Release>, String> {
        let file = tempfile::NamedTempFile::new().map_err(error)?;
        let url = format!(
            "https://api.github.com/repos/{}/releases?per_page=30",
            self.repository
        );
        if !download(&url, file.path(), 4 * 1024 * 1024, timeout, &cancel)? {
            return Ok(None);
        }
        select_release(
            self, file.reopen().map_err(error)?.take(4 * 1024 * 1024), kind, installed,
        )
    }

    pub fn prepare(
        &self,
        release: &Release,
        cancel: impl Fn() -> bool,
    ) -> Result<Option<Prepared>, String> {
        let root = self.store()?;
        fs::create_dir_all(&root).map_err(error)?;
        let stage = tempfile::tempdir_in(&root).map_err(error)?;
        let archive = stage.path().join("package.zip");
        if !download(&release.url, &archive, release.size, 300, &cancel)? {
            return Ok(None);
        }
        self.prepare_archive(release, stage, &archive, &cancel)
    }

    fn prepare_archive(
        &self,
        release: &Release,
        stage: tempfile::TempDir,
        archive: &Path,
        cancel: &impl Fn() -> bool,
    ) -> Result<Option<Prepared>, String> {
        let mut file = File::open(archive).map_err(error)?;
        if file.metadata().map_err(error)?.len() != release.size
            || !verify_digest(&mut file, &release.digest, &cancel)?
        {
            if cancel() {
                return Ok(None);
            }
            return Err("The download did not match GitHub's release checksum.".into());
        }
        file.rewind().map_err(error)?;
        let image = stage.path().join("image");
        if !extract(file, &image, &cancel)? {
            return Ok(None);
        }
        let package = package_root(&image)?;
        let channel = Self::read(&package)?;
        if channel.repository != self.repository || channel.version != release.version {
            return Err(
                "The downloaded package belongs to a different version or update channel.".into(),
            );
        }
        fs::remove_file(archive).map_err(error)?;
        Ok((!cancel()).then_some(Prepared {
            _stage: stage,
            package,
            channel,
        }))
    }

    pub fn redirect(&self, project: Option<&Path>) -> Result<bool, String> {
        let root = self.store()?;
        let pointer = root.join("active.json");
        if !pointer.exists() {
            return Ok(false);
        }
        let target: String = read_pointer(&pointer)?;
        let version = Version::parse(&target).map_err(error)?;
        if !version
            .cmp_precedence(&Version::parse(&self.version).map_err(error)?)
            .is_gt()
        {
            return Ok(false);
        }
        let stage = root.join(target);
        validate_package(&stage)?;
        let channel = Self::read(&stage)?;
        if channel.repository != self.repository
            || Version::parse(&channel.version).ok() != Some(version)
        {
            return Err("The active Mapper update is unavailable.".into());
        }
        launch(&stage, project)?;
        Ok(true)
    }
}

pub struct Prepared {
    _stage: tempfile::TempDir,
    package: PathBuf,
    channel: Channel,
}

impl Prepared {
    pub fn version(&self) -> &str {
        &self.channel.version
    }

    pub fn restart(&mut self, project: Option<&Path>) -> Result<(), String> {
        let root = self.channel.store()?;
        self.activate(&root, |path| launch(path, project))
    }

    fn activate(
        &mut self,
        root: &Path,
        open: impl FnOnce(&Path) -> Result<(), String>,
    ) -> Result<(), String> {
        let installed = root.join(&self.channel.version);
        if !installed.exists() {
            fs::rename(&self.package, &installed).map_err(error)?;
        }
        validate_package(&installed)?;
        if Channel::read(&installed)? != self.channel {
            return Err("An incompatible package already occupies this update version.".into());
        }
        let pointer = root.join("active.json");
        let previous = read_pointer(&pointer).ok();
        if previous
            .as_deref()
            .and_then(|value| Version::parse(value).ok())
            .is_some_and(|active| {
                active > Version::parse(&self.channel.version).expect("validated version")
            })
        {
            return Err("A newer Mapper update is already active. Reopen Mapper to use it.".into());
        }
        crate::profile::write_json(&pointer, &self.channel.version)?;
        if let Err(message) = open(&installed) {
            let rollback = if let Some(previous) = previous {
                crate::profile::write_json(&pointer, &previous)
            } else {
                fs::remove_file(&pointer).map_err(error)
            };
            rollback?;
            return Err(message);
        }
        Ok(())
    }
}

fn error(value: impl std::fmt::Display) -> String {
    value.to_string()
}

fn valid_repo(repo: &str) -> bool {
    repo.split_once('/').is_some_and(|(owner, name)| {
        [owner, name].into_iter().all(|token| {
            !token.is_empty()
                && token.len() <= 100
                && !matches!(token, "." | "..")
                && token
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        })
    })
}

fn read_pointer(path: &Path) -> Result<String, String> {
    serde_json::from_reader(File::open(path).map_err(error)?.take(512)).map_err(error)
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    state: String,
    size: u64,
    digest: Option<String>,
}

#[derive(Clone, Copy)]
enum ReleaseKind {
    Portable,
    Installer,
}

fn select_release(
    channel: &Channel,
    response: impl Read,
    kind: ReleaseKind,
    installed: Option<&Version>,
) -> Result<Option<Release>, String> {
    let current = Version::parse(&channel.version).map_err(error)?;
    let releases: Vec<GitHubRelease> = serde_json::from_reader(response).map_err(error)?;
    Ok(releases
        .into_iter()
        .filter_map(|entry| {
            let version = Version::parse(entry.tag_name.strip_prefix('v')?).ok()?;
            if entry.draft
                || !version.cmp_precedence(&current).is_gt()
                || installed.is_some_and(|old| !version.cmp_precedence(old).is_gt())
                || !version.build.is_empty()
                || (current.pre.is_empty() && (entry.prerelease || !version.pre.is_empty()))
            {
                return None;
            }
            let installer_name = format!("Mapper-for-Rust-{version}-Setup-win-x64.exe");
            let asset = entry.assets.into_iter().find(|asset| {
                asset.state == "uploaded"
                    && (1..=MAX_DOWNLOAD).contains(&asset.size)
                    && match kind {
                        ReleaseKind::Portable => asset.name.starts_with("Mapper-for-Rust-")
                            && asset.name.ends_with("-win-x64.zip"),
                        ReleaseKind::Installer => asset.name == installer_name,
                    }
                    && asset.name.len() <= 120
                    && asset
                        .name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
                    && asset.digest.as_deref().and_then(parse_digest).is_some()
            })?;
            let digest = parse_digest(asset.digest.as_deref()?)?.to_owned();
            let url = format!(
                "https://github.com/{}/releases/download/{}/{}",
                channel.repository, entry.tag_name, asset.name
            );
            Some((
                version,
                Release {
                    version: entry.tag_name[1..].to_owned(),
                    url,
                    size: asset.size,
                    digest,
                },
            ))
        })
        .max_by(|left, right| left.0.cmp_precedence(&right.0))
        .map(|(_, release)| release))
}

fn parse_digest(value: &str) -> Option<&str> {
    let hash = value.strip_prefix("sha256:")?;
    (hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(hash)
}

fn verify_digest(
    file: &mut File,
    digest: &str,
    cancel: &impl Fn() -> bool,
) -> Result<bool, String> {
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 256 * 1024];
    while !cancel() {
        let read = file.read(&mut buffer).map_err(error)?;
        if read == 0 {
            return Ok(format!("{:x}", hash.finalize()).eq_ignore_ascii_case(digest));
        }
        hash.update(&buffer[..read]);
    }
    Ok(false)
}

fn download(
    url: &str,
    path: &Path,
    limit: u64,
    seconds: u32,
    cancel: &impl Fn() -> bool,
) -> Result<bool, String> {
    if cancel() {
        return Ok(false);
    }
    let system =
        std::env::var_os("SystemRoot").ok_or("Windows update transport is unavailable.")?;
    let program = PathBuf::from(system).join("System32/curl.exe");
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    command
        .args([
            "--disable",
            "--silent",
            "--fail",
            "--location",
            "--max-redirs",
            "5",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--user-agent",
            "Mapper-for-Rust",
            "--max-time",
            &seconds.to_string(),
            "--max-filesize",
            &limit.to_string(),
            "--output",
        ])
        .arg(path)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut process = command.spawn().map_err(error)?;
    loop {
        if cancel() {
            let _ = process.kill();
            let _ = process.wait();
            return Ok(false);
        }
        if let Some(status) = process.try_wait().map_err(error)? {
            return if status.success() && fs::metadata(path).map_err(error)?.len() <= limit {
                Ok(true)
            } else {
                Err("Can't download Mapper updates. Check the connection and confirm the release repository is public.".into())
            };
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn extract(file: File, root: &Path, cancel: &impl Fn() -> bool) -> Result<bool, String> {
    let mut zip = zip::ZipArchive::new(file).map_err(error)?;
    if zip.is_empty() || zip.len() > 20_000 {
        return Err("Invalid update archive".into());
    }
    fs::create_dir(root).map_err(error)?;
    let mut total = 0_u64;
    let mut buffer = vec![0; 256 * 1024];
    for i in 0..zip.len() {
        if cancel() {
            return Ok(false);
        }
        let mut item = zip.by_index(i).map_err(error)?;
        let path = item
            .enclosed_name()
            .filter(|path| {
                path.components().all(|component| {
                    matches!(component, Component::Normal(name) if name.to_str().is_some_and(|s|
                !s.contains(':') && !s.contains('\\') && !s.ends_with([' ','.'])
                && !s.chars().any(char::is_control) && !reserved(s)))
                })
            })
            .ok_or("Update archive contains an unsafe file path")?;
        if path.as_os_str().is_empty() || item.is_symlink() {
            return Err("Update archive contains an invalid entry".into());
        }
        let destination = root.join(path);
        if item.is_dir() {
            fs::create_dir_all(&destination).map_err(error)?;
            continue;
        }
        fs::create_dir_all(destination.parent().ok_or("Invalid update path")?).map_err(error)?;
        let mut output = File::create_new(destination).map_err(error)?;
        let mut entry_bytes = 0;
        loop {
            if cancel() {
                return Ok(false);
            }
            let read = item.read(&mut buffer).map_err(error)?;
            if read == 0 {
                break;
            }
            entry_bytes += read as u64;
            total += read as u64;
            if entry_bytes > item.size() || total > MAX_EXPANDED {
                return Err("Update archive exceeds its expanded size limit".into());
            }
            output.write_all(&buffer[..read]).map_err(error)?;
        }
        if entry_bytes != item.size() {
            return Err("Incomplete update archive entry".into());
        }
    }
    Ok(true)
}

fn reserved(name: &str) -> bool {
    let upper = name
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end()
        .to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            upper.strip_prefix(prefix).is_some_and(|suffix| {
                [
                    "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "¹", "²", "³",
                ]
                .contains(&suffix)
            })
        })
}

fn package_root(image: &Path) -> Result<PathBuf, String> {
    if image.join(CHANNEL_FILE).is_file() {
        validate_package(image)?;
        return Ok(image.to_owned());
    }
    let mut entries = fs::read_dir(image).map_err(error)?;
    let only = entries
        .next()
        .transpose()
        .map_err(error)?
        .ok_or("Empty Mapper update")?
        .path();
    if entries.next().is_some() {
        return Err("Invalid Mapper update package layout".into());
    }
    validate_package(&only)?;
    Ok(only)
}

fn validate_package(path: &Path) -> Result<(), String> {
    if REQUIRED.iter().all(|file| path.join(file).is_file()) {
        Ok(())
    } else {
        Err("The update package is missing required Mapper files.".into())
    }
}

fn launch(path: &Path, project: Option<&Path>) -> Result<(), String> {
    let project = project
        .map(std::path::absolute)
        .transpose()
        .map_err(error)?;
    Command::new(path.join("mapper-studio.exe"))
        .args(project)
        .current_dir(path)
        .spawn()
        .map_err(error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use zip::write::SimpleFileOptions;

    #[test]
    fn github_preview_channel_picks_latest_compatible_asset() {
        let hash = format!("sha256:{}", "a".repeat(64));
        let release = |tag: &str, draft, prerelease, asset: &str| {
            json!({
                "tag_name":tag, "draft":draft, "prerelease":prerelease,
                "assets":[{"name":asset,"size":500,"state":"uploaded","digest":hash}]
            })
        };
        let asset = "Mapper-for-Rust-Preview-win-x64.zip";
        let data = json!([
            release("alpha-alpha", false, true, asset),
            release("v0.1.0-preview.900", true, true, asset),
            release("v0.1.0-preview.80", false, true, "Mapper-for-Rust-bad.zip"),
            release("v0.1.0-preview.10", false, true, asset),
            release("v0.1.0-preview.67", false, true, asset),
            release("v0.1.0-preview.8", false, true, asset),
        ])
        .to_string();
        let mut channel = Channel {
            repository: "Example/Mapper".into(),
            version: "0.1.0-preview.9".into(),
        };
        let latest = select_release(&channel, data.as_bytes(), ReleaseKind::Portable, None).unwrap().unwrap();
        assert_eq!(latest.version, "0.1.0-preview.67");
        assert!(latest.url.ends_with(&format!("/v0.1.0-preview.67/{asset}")));
        channel.version = "0.1.0".into();
        assert!(select_release(&channel, data.as_bytes(), ReleaseKind::Portable, None).unwrap().is_none());
    }

    #[test]
    fn setup_checks_versioned_exes_against_both_installer_and_installed_versions() {
        let setup = |version: &str, digest: Option<String>| {
            json!({
                "tag_name": format!("v{version}"), "draft": false, "prerelease": true,
                "assets": [
                    {"name":format!("Mapper-for-Rust-{version}-win-x64.zip"),"size":500,"state":"uploaded","digest":digest},
                    {"name":format!("Mapper-for-Rust-{version}-Setup-win-x64.exe"),"size":500,"state":"uploaded","digest":digest},
                ]
            })
        };
        let digest = Some(format!("sha256:{}", "a".repeat(64)));
        let data = json!([
            setup("0.1.0-beta.5", digest.clone()),
            setup("0.1.0-beta.10", digest),
            setup("0.1.0-beta.11", None),
        ]).to_string();
        let channel = Channel { repository: "Example/Mapper".into(), version: "0.1.0-beta.4".into() };
        let old = Version::parse("0.1.0-beta.9").unwrap();
        let release = select_release(&channel, data.as_bytes(), ReleaseKind::Installer, Some(&old)).unwrap().unwrap();
        assert_eq!(release.version, "0.1.0-beta.10");
        assert!(release.url.ends_with("/v0.1.0-beta.10/Mapper-for-Rust-0.1.0-beta.10-Setup-win-x64.exe"));
        let current = Version::parse("0.1.0-beta.10").unwrap();
        assert!(select_release(&channel, data.as_bytes(), ReleaseKind::Installer, Some(&current)).unwrap().is_none());
    }

    #[test]
    #[cfg(all(windows, target_arch = "x86_64"))]
    fn archive_install_checks_identity_and_rolls_back_failed_launch() {
        let root = tempfile::tempdir().unwrap();
        let channel = Channel {
            repository: "Example/Mapper".into(),
            version: "0.1.0-preview.1".into(),
        };
        let fixture = |unsafe_name: bool| {
            let stage = tempfile::tempdir_in(root.path()).unwrap();
            let archive = stage.path().join("package.zip");
            let mut zip = zip::ZipWriter::new(File::create(&archive).unwrap());
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            for name in REQUIRED {
                zip.start_file(format!("Mapper/{name}"), options).unwrap();
                zip.write_all(b"fixture").unwrap();
            }
            zip.start_file("Mapper/update-channel.json", options)
                .unwrap();
            zip.write_all(br#"{"repository":"Example/Mapper","version":"0.1.0-preview.2"}"#)
                .unwrap();
            if unsafe_name {
                zip.start_file("Mapper/../../outside", options).unwrap();
                zip.write_all(b"unsafe test entry").unwrap();
            }
            zip.finish().unwrap();
            let bytes = fs::read(&archive).unwrap();
            let release = Release {
                version: "0.1.0-preview.2".into(),
                url: String::new(),
                size: bytes.len() as u64,
                digest: format!("{:x}", Sha256::digest(&bytes)),
            };
            (stage, archive, release)
        };
        let (stage, archive, mut release) = fixture(false);
        release.digest = "0".repeat(64);
        assert!(
            channel
                .prepare_archive(&release, stage, &archive, &|| false)
                .err()
                .unwrap()
                .contains("checksum")
        );
        let (stage, archive, release) = fixture(true);
        assert!(
            channel
                .prepare_archive(&release, stage, &archive, &|| false)
                .is_err()
        );
        assert!(!root.path().join("outside").exists());
        let (stage, archive, release) = fixture(false);
        assert!(
            channel
                .prepare_archive(&release, stage, &archive, &|| true)
                .unwrap()
                .is_none()
        );
        assert!(!root.path().join("active.json").exists());
        let (stage, archive, release) = fixture(false);
        let mut prepared = channel
            .prepare_archive(&release, stage, &archive, &|| false)
            .unwrap()
            .unwrap();
        assert!(!root.path().join("active.json").exists());
        assert!(
            prepared
                .activate(root.path(), |_| Err("Launch failed".into()))
                .is_err()
        );
        assert!(!root.path().join("active.json").exists());
        prepared
            .activate(root.path(), |path| {
                assert_eq!(path.file_name().unwrap(), "0.1.0-preview.2");
                assert!(path.join("mapper-studio.exe").is_file());
                Ok(())
            })
            .unwrap();
        assert_eq!(
            read_pointer(&root.path().join("active.json")).unwrap(),
            "0.1.0-preview.2"
        );
    }
}
