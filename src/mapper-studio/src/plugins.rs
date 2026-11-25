//! Explicit, crash-isolated local recipe plugins. Processes are NOT sandboxed.
use crate::{MAX_PROJECT_BYTES, Project};
pub use mapper_plugin_sdk::{Finding, Report, Severity};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    time::{Duration, Instant},
};

pub const API_VERSION: u32 = 1;
pub const MAX_MESSAGE: usize = MAX_PROJECT_BYTES as usize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisteredPackage {
    pub id: String,
    pub name: String,
    path: PathBuf,
    fingerprint: String,
}

impl RegisteredPackage {
    pub fn inspect(&self, cancelled: impl Fn() -> bool) -> Result<Loaded, String> {
        let plugin = Loaded::load_cancellable(&self.path, cancelled)?;
        if plugin.fingerprint() != self.fingerprint || plugin.manifest.id != self.id {
            return Err("Package changed. Add its manifest again to accept the update.".into());
        }
        Ok(plugin)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryDocument {
    schema: u32,
    packages: Vec<RegisteredPackage>,
}

pub struct Registry {
    path: PathBuf,
    document: RegistryDocument,
}

impl Registry {
    pub fn load(path: PathBuf) -> Result<Self, String> {
        let document = Self::read(&path)?;
        Ok(Self { path, document })
    }

    fn read(path: &Path) -> Result<RegistryDocument, String> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RegistryDocument {
                    schema: 1,
                    packages: Vec::new(),
                });
            }
            Err(error) => return Err(error.to_string()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_PROJECT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() as u64 > MAX_PROJECT_BYTES {
            return Err("Plugin list exceeds 4 MiB".into());
        }
        let document: RegistryDocument =
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        let mut ids = std::collections::HashSet::new();
        let mut paths = std::collections::HashSet::new();
        if document.schema != 1
            || document.packages.iter().any(|package| {
                !identifier(&package.id)
                    || !label(&package.name, 128)
                    || !package.path.is_absolute()
                    || !fingerprint(&package.fingerprint)
                    || !ids.insert(&package.id)
                    || !paths.insert(&package.path)
            })
        {
            return Err("Unsupported or invalid plugin list".into());
        }
        Ok(document)
    }

    pub fn packages(&self) -> &[RegisteredPackage] {
        &self.document.packages
    }

    pub fn register(&mut self, plugin: &Loaded) -> Result<usize, String> {
        let package = RegisteredPackage {
            id: plugin.manifest.id.clone(),
            name: plugin.manifest.name.clone(),
            path: plugin.path.clone(),
            fingerprint: plugin.receipt.clone(),
        };
        let mut next = self.document.clone();
        if next.packages.iter().any(|entry| entry.path == package.path && entry.id != package.id) {
            return Err("Package ID changed. Remove the old entry before adding it again.".into());
        }
        let index = next.packages.iter().position(|entry| entry.id == package.id);
        let index = match index {
            Some(index) => {
                next.packages[index] = package;
                index
            }
            None => {
                next.packages.push(package);
                next.packages.len() - 1
            }
        };
        self.commit(next)?;
        Ok(index)
    }

    pub fn remove(&mut self, index: usize) -> Result<(), String> {
        let mut next = self.document.clone();
        if index >= next.packages.len() {
            return Err("Plugin entry no longer exists".into());
        }
        next.packages.remove(index);
        self.commit(next)
    }

    fn commit(&mut self, next: RegistryDocument) -> Result<(), String> {
        if next != self.document {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            crate::profile::write_document_checked(&self.path, &next, true, || {
                if Self::read(&self.path)? != self.document {
                    return Err("Plugin list changed in another window. Reopen Mapper and try again.".into());
                }
                Ok(())
            })?;
            self.document = next;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<Capability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Vec<Parameter>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Capability {
    #[serde(rename = "recipe.transform")]
    Transform,
    #[serde(rename = "recipe.inspect")]
    Inspect,
}
impl Capability {
    fn name(self) -> &'static str {
        match self {
            Self::Transform => "recipe.transform",
            Self::Inspect => "recipe.inspect",
        }
    }
}

pub enum Outcome {
    Recipe(Box<Project>),
    Report(Report),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Parameter {
    pub id: String,
    pub label: String,
    pub control: Control,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Control {
    Boolean {
        default: bool,
    },
    Integer {
        default: i32,
        min: i32,
        max: i32,
    },
    Number {
        default: f64,
        min: f64,
        max: f64,
    },
    Text {
        default: String,
        max_chars: usize,
    },
    Choice {
        default: String,
        options: Vec<String>,
    },
}

impl Control {
    pub fn default_value(&self) -> Value {
        match self {
            Self::Boolean { default } => Value::from(*default),
            Self::Integer { default, .. } => Value::from(*default),
            Self::Number { default, .. } => Value::from(*default),
            Self::Text { default, .. } | Self::Choice { default, .. } => {
                Value::from(default.clone())
            }
        }
    }

    fn accepts(&self, value: &Value) -> bool {
        match self {
            Self::Boolean { .. } => value.is_boolean(),
            Self::Integer { min, max, .. } => value
                .as_i64()
                .is_some_and(|n| (i64::from(*min)..=i64::from(*max)).contains(&n)),
            Self::Number { min, max, .. } => value
                .as_f64()
                .is_some_and(|n| n.is_finite() && (*min..=*max).contains(&n)),
            Self::Text { max_chars, .. } => value.as_str().is_some_and(|s| {
                s.chars().count() <= *max_chars && !s.chars().any(char::is_control)
            }),
            Self::Choice { options, .. } => value
                .as_str()
                .is_some_and(|s| options.iter().any(|option| option == s)),
        }
    }

    fn valid(&self) -> bool {
        let definition_valid = match self {
            Self::Number { min, max, .. } => min.is_finite() && max.is_finite(),
            Self::Text { max_chars, .. } => (1..=4096).contains(max_chars),
            Self::Choice { options, .. } => {
                (1..=32).contains(&options.len())
                    && options.iter().all(|name| label(name, 128))
                    && options
                        .iter()
                        .collect::<std::collections::HashSet<_>>()
                        .len()
                        == options.len()
            }
            _ => true,
        };
        definition_valid && self.accepts(&self.default_value())
    }
}

impl Action {
    pub fn kind(&self) -> Capability {
        self.capability.unwrap_or(Capability::Transform)
    }

    fn valid_parameters(&self) -> bool {
        self.parameters.as_ref().is_none_or(|fields| {
            let mut ids = std::collections::HashSet::new();
            fields.len() <= 32
                && fields.iter().all(|field| {
                    identifier(&field.id)
                        && label(&field.label, 128)
                        && ids.insert(&field.id)
                        && field.control.valid()
                })
        })
    }

    fn resolve_parameters(&self, parameters: Value) -> Result<Value, String> {
        let Some(fields) = &self.parameters else {
            return Ok(parameters);
        };
        let Value::Object(mut submitted) = parameters else {
            return Err("This plugin action requires a JSON object for its parameters".into());
        };
        let mut resolved = Map::new();
        for field in fields {
            let value = submitted
                .remove(&field.id)
                .unwrap_or_else(|| field.control.default_value());
            if !field.control.accepts(&value) {
                return Err(format!("Invalid value for plugin parameter '{}'", field.id));
            }
            resolved.insert(field.id.clone(), value);
        }
        if let Some(id) = submitted.keys().next() {
            return Err(format!("Unknown plugin parameter '{id}'"));
        }
        Ok(Value::Object(resolved))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: String,
    pub api_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub executable: String,
    pub executable_sha256: String,
    pub capabilities: Vec<String>,
    pub actions: Vec<Action>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub protocol: u32,
    pub id: u64,
    pub action: String,
    pub parameters: serde_json::Value,
    pub project: Project,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub protocol: u32,
    pub id: u64,
    pub project: Option<Project>,
    pub error: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportResponse {
    protocol: u32,
    id: u64,
    report: Option<Report>,
    error: Option<String>,
}

fn identifier(s: &str) -> bool {
    (1..=128).contains(&s.len())
        && s.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b".-_".contains(&b))
}
fn label(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}
fn fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
impl Manifest {
    pub fn validate(&self) -> Result<(), String> {
        let mut ids = std::collections::HashSet::new();
        let mut capabilities = std::collections::HashSet::new();
        if self.format != "papyrus-mapper-plugin"
            || self.api_version != API_VERSION
            || !identifier(&self.id)
            || !label(&self.name, 128)
            || !label(&self.version, 64)
            || !label(&self.description, 1024)
            || self.capabilities.is_empty()
            || self.capabilities.iter().any(|name| {
                !matches!(name.as_str(), "recipe.transform" | "recipe.inspect")
                    || !capabilities.insert(name.as_str())
            })
            || self.actions.is_empty()
            || self.actions.len() > 32
            || self.actions.iter().any(|a| {
                !identifier(&a.id)
                    || !label(&a.title, 128)
                    || !label(&a.description, 1024)
                    || !ids.insert(&a.id)
                    || !a.valid_parameters()
                    || !capabilities.contains(a.kind().name())
            })
        {
            return Err("Unsupported or invalid Mapper plugin manifest".into());
        }
        let path = Path::new(&self.executable);
        if cfg!(windows)
            && !path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
        {
            return Err("Windows plugins require a direct .exe entry point".into());
        }
        if self.executable.is_empty()
            || self.executable.len() > 512
            || self.executable.contains(['\\', ':', '\0'])
            || path
                .components()
                .any(|p| !matches!(p, Component::Normal(_)))
            || !fingerprint(&self.executable_sha256)
        {
            return Err(
                "Plugin executable must be a contained relative path with a SHA-256 fingerprint"
                    .into(),
            );
        }
        Ok(())
    }
}

pub struct Loaded {
    pub manifest: Manifest,
    path: PathBuf,
    executable: PathBuf,
    receipt: String,
}
impl Loaded {
    /// Inspection validates metadata and hashes files; it never executes a plugin.
    pub fn load(path: &Path) -> Result<Self, String> {
        Self::load_cancellable(path, || false)
    }
    pub fn load_cancellable(path: &Path, cancelled: impl Fn() -> bool) -> Result<Self, String> {
        if cancelled() {
            return Err("Plugin inspection cancelled".into());
        }
        let path = path.canonicalize().map_err(|e| e.to_string())?;
        let mut raw = Vec::new();
        File::open(&path)
            .map_err(|e| e.to_string())?
            .take(65537)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        if raw.len() > 65536 {
            return Err("Plugin manifest exceeds 64 KiB".into());
        }
        let manifest: Manifest = serde_json::from_slice(&raw).map_err(|e| e.to_string())?;
        manifest.validate()?;
        let root = path.parent().ok_or("Missing plugin directory")?;
        let executable = root
            .join(&manifest.executable)
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if !executable.starts_with(root) || !executable.is_file() {
            return Err("Plugin executable escapes its package".into());
        }
        let mut file = File::open(&executable).map_err(|e| e.to_string())?;
        if file.metadata().map_err(|e| e.to_string())?.len() > 256 * 1024 * 1024 {
            return Err("Plugin executable exceeds 256 MiB".into());
        }
        let mut hash = Sha256::new();
        let mut buffer = [0; 65536];
        let mut read_bytes = 0_u64;
        loop {
            if cancelled() {
                return Err("Plugin inspection cancelled".into());
            }
            let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            read_bytes += n as u64;
            if read_bytes > 256 * 1024 * 1024 {
                return Err("Plugin executable grew beyond the inspection budget".into());
            }
            hash.update(&buffer[..n]);
        }
        if format!("{:x}", hash.finalize()) != manifest.executable_sha256 {
            return Err("Plugin executable fingerprint mismatch".into());
        }
        Ok(Self {
            manifest,
            path,
            executable,
            receipt: format!("{:x}", Sha256::digest(&raw)),
        })
    }
    pub fn manifest_path(&self) -> &Path {
        &self.path
    }
    pub fn fingerprint(&self) -> &str {
        &self.receipt
    }
    pub fn run(
        &self,
        action: &str,
        project: &Project,
        parameters: serde_json::Value,
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> Result<Project, String> {
        match self.invoke(
            action,
            Capability::Transform,
            project,
            parameters,
            timeout,
            cancel,
        )? {
            Outcome::Recipe(recipe) => Ok(*recipe),
            Outcome::Report(_) => Err("Expected a recipe replacement".into()),
        }
    }

    pub fn invoke(
        &self,
        action: &str,
        capability: Capability,
        project: &Project,
        parameters: serde_json::Value,
        timeout: Duration,
        cancel: &AtomicBool,
    ) -> Result<Outcome, String> {
        project.validate()?;
        if cancel.load(Ordering::Relaxed) {
            return Err("Plugin cancelled".into());
        }
        if !(Duration::from_millis(100)..=Duration::from_secs(60)).contains(&timeout) {
            return Err("Plugin timeout must be 100 ms to 60 seconds".into());
        }
        let current = Self::load_cancellable(&self.path, || cancel.load(Ordering::Relaxed))?;
        if current.receipt != self.receipt || current.executable != self.executable {
            return Err("Plugin package changed; inspect it again before running".into());
        }
        let definition = current
            .manifest
            .actions
            .iter()
            .find(|a| a.id == action)
            .ok_or("Unknown plugin action")?;
        if definition.kind() != capability {
            return Err(format!(
                "Plugin action requires {}",
                definition.kind().name()
            ));
        }
        let parameters = definition.resolve_parameters(parameters)?;
        if serde_json::to_vec(&parameters)
            .map_err(|e| e.to_string())?
            .len()
            > 65536
        {
            return Err("Plugin parameters exceed 64 KiB".into());
        }
        let request = Request {
            protocol: API_VERSION,
            id: 1,
            action: action.into(),
            parameters,
            project: project.clone(),
        };
        let raw = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
        if raw.len() > MAX_MESSAGE {
            return Err("Plugin request exceeds 4 MiB".into());
        }
        let mut command = Command::new(&self.executable);
        command
            .current_dir(self.path.parent().unwrap())
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in [
            "PATH",
            "SystemRoot",
            "WINDIR",
            "TEMP",
            "TMP",
            "LANG",
            "LC_ALL",
        ] {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        if cancel.load(Ordering::Relaxed) {
            return Err("Plugin cancelled".into());
        }
        let mut child = Running(
            command
                .spawn()
                .map_err(|e| format!("Cannot start plugin: {e}"))?,
        );
        let mut stdin = child.0.stdin.take().ok_or("Missing plugin input pipe")?;
        let (writer, write_result) = mpsc::channel();
        std::thread::spawn(move || {
            let result = stdin
                .write_all(&raw)
                .and_then(|_| stdin.write_all(b"\n"))
                .map_err(|e| e.to_string());
            drop(stdin);
            let _ = writer.send(result);
        });
        let overflow = Arc::new(AtomicBool::new(false));
        let output = capture(
            child.0.stdout.take().ok_or("Missing plugin output pipe")?,
            MAX_MESSAGE,
            overflow.clone(),
        );
        let errors = capture(
            child.0.stderr.take().ok_or("Missing plugin error pipe")?,
            65536,
            overflow.clone(),
        );
        let started = Instant::now();
        let mut stdout = None;
        let mut stderr = None;
        let mut written = None;
        let mut status = None;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("Plugin cancelled".into());
            }
            if overflow.load(Ordering::Relaxed) {
                return Err("Plugin exceeded its output budget".into());
            }
            if started.elapsed() > timeout {
                return Err("Plugin timed out".into());
            }
            if stdout.is_none() {
                stdout = poll(&output)?;
            }
            if stderr.is_none() {
                stderr = poll(&errors)?;
            }
            if written.is_none() {
                written = poll(&write_result)?;
            }
            if status.is_none() {
                status = child.0.try_wait().map_err(|e| e.to_string())?;
            }
            if let (Some(out), Some(err), Some(_), Some(status)) =
                (&stdout, &stderr, &written, &status)
            {
                if !status.success() {
                    return Err(format!(
                        "Plugin exited unsuccessfully: {}",
                        String::from_utf8_lossy(err)
                            .chars()
                            .take(2048)
                            .collect::<String>()
                    ));
                }
                return match capability {
                    Capability::Transform => validate_response(out, request.id)
                        .map(|recipe| Outcome::Recipe(Box::new(recipe))),
                    Capability::Inspect => validate_report(out, request.id).map(Outcome::Report),
                };
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn capture(
    reader: impl Read + Send + 'static,
    limit: usize,
    overflow: Arc<AtomicBool>,
) -> Receiver<Result<Vec<u8>, String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = reader
            .take((limit + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())
            .and_then(|_| {
                if bytes.len() > limit {
                    overflow.store(true, Ordering::Relaxed);
                    Err("Plugin output limit exceeded".into())
                } else {
                    Ok(bytes)
                }
            });
        let _ = sender.send(result);
    });
    receiver
}
fn poll<T>(receiver: &Receiver<Result<T, String>>) -> Result<Option<T>, String> {
    match receiver.try_recv() {
        Ok(result) => result.map(Some),
        Err(TryRecvError::Empty) => Ok(None),
        Err(TryRecvError::Disconnected) => Err("Plugin transport ended unexpectedly".into()),
    }
}
fn validate_response(bytes: &[u8], id: u64) -> Result<Project, String> {
    let response: Response =
        serde_json::from_slice(bytes).map_err(|e| format!("Invalid plugin response: {e}"))?;
    if response.protocol != API_VERSION || response.id != id {
        return Err("Plugin response protocol/request mismatch".into());
    }
    match (response.project, response.error) {
        (Some(project), None) => {
            project.validate()?;
            Ok(project)
        }
        (None, Some(error)) if !error.trim().is_empty() && error.len() <= 4096 => {
            Err(format!("Plugin: {error}"))
        }
        _ => Err("Plugin must return exactly one project or error".into()),
    }
}

fn validate_report(bytes: &[u8], id: u64) -> Result<Report, String> {
    let response: ReportResponse =
        serde_json::from_slice(bytes).map_err(|e| format!("Invalid plugin response: {e}"))?;
    if response.protocol != API_VERSION || response.id != id {
        return Err("Plugin response protocol/request mismatch".into());
    }
    match (response.report, response.error) {
        (Some(report), None) => {
            report.validate()?;
            Ok(report)
        }
        (None, Some(error)) if !error.trim().is_empty() && error.len() <= 4096 => {
            Err(format!("Plugin: {error}"))
        }
        _ => Err("Plugin must return exactly one report or error".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn manifest() -> Manifest {
        Manifest {
            format: "papyrus-mapper-plugin".into(),
            api_version: 1,
            id: "example.test".into(),
            name: "Test".into(),
            version: "0.1.0".into(),
            description: "Test plugin".into(),
            executable: "plugin.exe".into(),
            executable_sha256: "a".repeat(64),
            capabilities: vec!["recipe.transform".into()],
            actions: vec![Action {
                id: "change".into(),
                title: "Change recipe".into(),
                description: "Change seeds".into(),
                capability: None,
                parameters: None,
            }],
        }
    }
    #[test]
    fn saved_registry_keeps_multiple_plugins_and_requires_explicit_package_updates() {
        let directory = tempfile::tempdir().unwrap();
        let registry_path = directory.path().join("plugins.json");
        let install = |name: &str, version: &str| {
            let path = directory.path().join(name);
            std::fs::create_dir_all(&path).unwrap();
            let binary = b"inspection does not execute the package";
            std::fs::write(path.join("plugin.exe"), binary).unwrap();
            let path = path.join("mapper-plugin.json");
            let mut manifest = manifest();
            manifest.id = format!("example.{name}");
            manifest.version = version.into();
            manifest.executable_sha256 = format!("{:x}", Sha256::digest(binary));
            std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
            Loaded::load(&path).unwrap()
        };
        let mut registry = Registry::load(registry_path.clone()).unwrap();
        assert_eq!(registry.register(&install("first", "1")).unwrap(), 0);
        assert_eq!(registry.register(&install("second", "1")).unwrap(), 1);

        let mut restarted = Registry::load(registry_path.clone()).unwrap();
        assert_eq!(restarted.packages(), registry.packages());
        assert_eq!(restarted.packages()[0].inspect(|| false).unwrap().manifest.version, "1");
        let newer = install("first", "2");
        assert!(restarted.packages()[0].inspect(|| false).err().unwrap().contains("Package changed"));
        assert_eq!(restarted.register(&newer).unwrap(), 0);
        assert!(registry.remove(1).is_err());
        assert_eq!(registry.packages().len(), 2);

        restarted.remove(0).unwrap();
        assert!(newer.manifest_path().is_file());
        let final_registry = Registry::load(registry_path).unwrap();
        assert_eq!(final_registry.packages()[0].id, "example.second");
        assert_eq!(final_registry.packages().len(), 1);
    }
    #[test]
    fn rejects_unknown_api_capabilities_paths_and_duplicate_actions() {
        let m = manifest();
        m.validate().unwrap();
        let mut bad = m.clone();
        bad.api_version = 99;
        assert!(bad.validate().is_err());
        bad = m.clone();
        bad.capabilities.push("native.execute".into());
        assert!(bad.validate().is_err());
        for path in [
            "../plugin.exe",
            "/plugin.exe",
            "C:/plugin.exe",
            "dir/../../plugin.exe",
            "dir\\plugin.exe",
        ] {
            bad = m.clone();
            bad.executable = path.into();
            assert!(bad.validate().is_err());
        }
        bad = m.clone();
        bad.actions.push(bad.actions[0].clone());
        assert!(bad.validate().is_err());
    }
    #[test]
    fn response_must_match_request_and_validate_recipe() {
        let mut response = Response {
            protocol: 1,
            id: 1,
            project: Some(Project::default()),
            error: None,
        };
        assert!(validate_response(&serde_json::to_vec(&response).unwrap(), 1).is_ok());
        assert!(validate_response(&serde_json::to_vec(&response).unwrap(), 2).is_err());
        response.project.as_mut().unwrap().size = 10;
        assert!(validate_response(&serde_json::to_vec(&response).unwrap(), 1).is_err());
        response.project = Some(Project::default());
        response.error = Some("No".into());
        assert!(validate_response(&serde_json::to_vec(&response).unwrap(), 1).is_err());
        assert!(validate_response(b"{}\n{}", 1).is_err());
    }

    #[test]
    fn declared_parameters_fill_defaults_and_reject_invalid_input() {
        let manifest: Manifest = serde_json::from_value(serde_json::json!({
            "format": "papyrus-mapper-plugin", "api_version": 1,
            "id": "test.controls", "name": "Controls", "version": "1",
            "description": "Recipe controls", "executable": "plugin.exe",
            "executable_sha256": "a".repeat(64), "capabilities": ["recipe.transform"],
            "actions": [{
                "id": "edit", "title": "Edit", "description": "Edit the recipe",
                "parameters": [
                    {"id":"enabled","label":"Enabled","control":{"type":"boolean","default":true}},
                    {"id":"count","label":"Count","control":{"type":"integer","default":1,"min":0,"max":3}},
                    {"id":"weight","label":"Weight","control":{"type":"number","default":0.5,"min":0,"max":1}},
                    {"id":"name","label":"Name","control":{"type":"text","default":"","max_chars":4}},
                    {"id":"mode","label":"Mode","control":{"type":"choice","default":"Balanced","options":["Balanced","Sparse"]}}
                ]
            }]
        })).unwrap();
        manifest.validate().unwrap();
        let action = &manifest.actions[0];
        assert_eq!(
            action
                .resolve_parameters(serde_json::json!({"mode":"Sparse","count":2}))
                .unwrap(),
            serde_json::json!({"enabled":true,"count":2,"weight":0.5,"name":"","mode":"Sparse"})
        );
        for invalid in [
            serde_json::json!([]),
            serde_json::json!({"missing":true}),
            serde_json::json!({"enabled":1}),
            serde_json::json!({"count":2.5}),
            serde_json::json!({"count":4}),
            serde_json::json!({"weight":-1}),
            serde_json::json!({"mode":"Unknown"}),
            serde_json::json!({"name":"a\nb"}),
            serde_json::json!({"name":"12345"}),
        ] {
            assert!(action.resolve_parameters(invalid).is_err());
        }
        let mut invalid = manifest;
        let fields = invalid.actions[0].parameters.as_mut().unwrap();
        fields[1].control = Control::Integer {
            default: 99,
            min: 0,
            max: 3,
        };
        assert!(invalid.validate().is_err());
    }
    #[test]
    fn discovery_checks_executable_without_running_it() {
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("plugin.exe");
        std::fs::write(&executable, b"not executable").unwrap();
        let mut m = manifest();
        m.executable_sha256 = format!("{:x}", Sha256::digest(b"not executable"));
        let path = root.path().join("mapper-plugin.json");
        std::fs::write(&path, serde_json::to_vec(&m).unwrap()).unwrap();
        let loaded = Loaded::load(&path).unwrap();
        assert_eq!(loaded.manifest.id, m.id);
        std::fs::write(&executable, b"changed").unwrap();
        assert!(Loaded::load(&path).is_err());
        assert!(
            loaded
                .run(
                    "change",
                    &Project::default(),
                    serde_json::json!({}),
                    Duration::from_secs(1),
                    &AtomicBool::new(false)
                )
                .is_err()
        );
    }
}
