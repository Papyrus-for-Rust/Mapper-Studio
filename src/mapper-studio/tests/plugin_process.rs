//! Real process/pipe tests. The std-only fixture never accesses user files/network.
use mapper_studio::{
    Project,
    plugins::{Action, Capability, Control, Loaded, Manifest, Parameter},
    project_bundle,
};
use sha2::{Digest, Sha256};
use std::{
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[test]
fn process_plugins_validate_output_and_stop_on_timeout_cancellation_or_flood() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("fixture.rs");
    std::fs::write(&source,r##"
use std::io::{Read,Write};
fn main(){
 let mut input=String::new();std::io::stdin().read_to_string(&mut input).unwrap();let input=input.trim();
 if input.contains("\"action\":\"timeout\""){std::thread::sleep(std::time::Duration::from_secs(30));}
 if input.contains("\"action\":\"flood\""){let _=std::io::stdout().write_all(&vec![b'x';5*1024*1024]);return;}
 if input.contains("\"action\":\"malformed\""){println!("not json");return;}
 if input.contains("\"action\":\"failure\""){eprintln!("fixture failed");std::process::exit(7);}
 if input.contains("\"action\":\"check-report\""){
    println!("{}",r#"{"protocol":1,"id":1,"report":{"summary":"Recipe check","findings":[{"severity":"warning","message":"Review monument preferences"}]},"error":null}"#);
    return;
 }
 if input.contains("\"action\":\"typed\"") && !input.contains("\"parameters\":{\"enabled\":true}") {std::process::exit(8);}
 if input.contains("\"action\":\"wait-for-edit\""){
    std::fs::write("started",b"").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !std::path::Path::new("resume").exists() {
        if std::time::Instant::now() >= deadline {std::process::exit(9);}
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
 }
 let start=input.find("\"project\":").unwrap()+10;let mut project=input[start..input.len()-1].to_string();
 if input.contains("\"action\":\"invalid\""){project=project.replace("\"size\":4000","\"size\":1");}
 if input.contains("\"action\":\"resize\""){project=project.replace("\"size\":4000","\"size\":2000");}
 let id=if input.contains("\"action\":\"wrong-id\""){2}else{1};
 println!("{{\"protocol\":1,\"id\":{},\"project\":{},\"error\":null}}",id,project);
}
"##).unwrap();
    let executable = root.path().join(if cfg!(windows) {
        "fixture.exe"
    } else {
        "fixture"
    });
    let status = Command::new("rustc")
        .arg("--edition=2024")
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .status()
        .unwrap();
    assert!(status.success());
    let actions = [
        "echo",
        "timeout",
        "flood",
        "malformed",
        "failure",
        "invalid",
        "wrong-id",
        "typed",
        "wait-for-edit",
        "resize",
        "check-report",
        "check-replacement",
    ];
    let manifest = Manifest {
        format: "papyrus-mapper-plugin".into(),
        api_version: 1,
        id: "test.fixture".into(),
        name: "Fixture".into(),
        version: "1".into(),
        description: "Transport fixture".into(),
        executable: executable.file_name().unwrap().to_string_lossy().into(),
        executable_sha256: format!("{:x}", Sha256::digest(std::fs::read(&executable).unwrap())),
        capabilities: vec!["recipe.transform".into(), "recipe.inspect".into()],
        actions: actions
            .iter()
            .map(|id| Action {
                id: (*id).into(),
                title: (*id).into(),
                description: "Test".into(),
                capability: id.starts_with("check-").then_some(Capability::Inspect),
                parameters: (*id == "typed").then(|| {
                    vec![Parameter {
                        id: "enabled".into(),
                        label: "Enabled".into(),
                        control: Control::Boolean { default: true },
                    }]
                }),
            })
            .collect(),
    };
    let path = root.path().join("mapper-plugin.json");
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let plugin = Loaded::load(&path).unwrap();
    let cancel = AtomicBool::new(false);
    let project = Project::default();
    assert_eq!(
        plugin
            .run(
                "typed",
                &project,
                serde_json::json!({}),
                Duration::from_secs(3),
                &cancel
            )
            .unwrap(),
        project
    );
    assert!(
        plugin
            .run(
                "typed",
                &project,
                serde_json::json!({"enabled":1}),
                Duration::from_secs(3),
                &cancel
            )
            .unwrap_err()
            .contains("Invalid value for plugin parameter")
    );
    assert_eq!(
        plugin
            .run(
                "echo",
                &project,
                serde_json::json!({}),
                Duration::from_secs(3),
                &cancel
            )
            .unwrap(),
        project
    );
    for action in ["flood", "malformed", "failure", "invalid", "wrong-id"] {
        assert!(
            plugin
                .run(
                    action,
                    &project,
                    serde_json::json!({}),
                    Duration::from_secs(3),
                    &cancel
                )
                .is_err(),
            "{action}"
        );
    }
    let clock = Instant::now();
    assert!(
        plugin
            .run(
                "timeout",
                &project,
                serde_json::json!({}),
                Duration::from_millis(100),
                &cancel
            )
            .unwrap_err()
            .contains("timed out")
    );
    assert!(clock.elapsed() < Duration::from_secs(3));
    let signal = Arc::new(AtomicBool::new(false));
    let setter = signal.clone();
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        setter.store(true, Ordering::Relaxed);
    });
    assert!(
        plugin
            .run(
                "timeout",
                &project,
                serde_json::json!({}),
                Duration::from_secs(3),
                &signal
            )
            .unwrap_err()
            .contains("cancelled")
    );
    thread.join().unwrap();

    for bundled in [false, true] {
        let source = root.path().join("project.mapper");
        let payload = project_bundle::Source::Bytes(Arc::from(b"saved world".as_slice()));
        if bundled {
            project_bundle::write(
                &source,
                project.clone(),
                serde_json::json!({"active_map":1}),
                std::slice::from_ref(&payload),
                || false,
            )
            .unwrap();
        } else {
            project.save(&source).unwrap();
        }
        let mut child = Command::new(env!("CARGO_BIN_EXE_mapper"))
            .args(["plugins", "run"])
            .arg(&path)
            .args(["wait-for-edit", "--project"])
            .arg(&source)
            .arg("--output")
            .arg(&source)
            .args(["--force", "--timeout", "8"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let started = root.path().join("started");
        let resume = root.path().join("resume");
        let deadline = Instant::now() + Duration::from_secs(4);
        while !started.exists() {
            if Instant::now() >= deadline || child.try_wait().unwrap().is_some() {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "Plugin did not reach the edit point: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if bundled {
            // A saved map tab can change even when the generation recipe is identical.
            project_bundle::write(
                &source,
                project.clone(),
                serde_json::json!({"active_map":22}),
                &[payload],
                || false,
            )
            .unwrap();
        } else {
            let mut edited = project.clone();
            edited.shuffle_monuments();
            edited.save(&source).unwrap();
        }
        let newest = std::fs::read(&source).unwrap();
        std::fs::write(&resume, b"").unwrap();
        let result = child.wait_with_output().unwrap();
        assert_eq!(result.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&result.stderr).contains("source project changed"));
        assert_eq!(std::fs::read(&source).unwrap(), newest);
        std::fs::remove_file(started).unwrap();
        std::fs::remove_file(resume).unwrap();
        if bundled {
            let output = root.path().join("updated.mapper");
            let result = Command::new(env!("CARGO_BIN_EXE_mapper"))
                .args(["plugins", "run"])
                .arg(&path)
                .args(["resize", "--project"])
                .arg(&source)
                .arg("--output")
                .arg(&output)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let updated = project_bundle::load(&output, || false).unwrap().unwrap();
            assert_eq!(updated.manifest.recipe.size, 2000);
            assert_eq!(
                updated.manifest.workspace,
                serde_json::json!({"active_map":22})
            );
            assert_eq!(
                std::fs::read(updated.blobs[0].as_ref()).unwrap(),
                b"saved world"
            );
        }
    }

    let plain = root.path().join("plain.mapper");
    let out = root.path().join("preserved.mapper");
    project.save(&plain).unwrap();
    let original = std::fs::read(&plain).unwrap();
    for (action, success) in [
        ("check-report", true),
        ("check-replacement", false),
        ("echo", false),
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_mapper"))
            .args(["plugins", "check"])
            .arg(&path)
            .args([action, "--project"])
            .arg(&plain)
            .output()
            .unwrap();
        assert_eq!(
            result.status.success(),
            success,
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        if success {
            let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
            assert_eq!(report["report"]["findings"][0]["severity"], "warning");
        } else {
            assert!(result.stdout.is_empty());
        }
        assert_eq!(std::fs::read(&plain).unwrap(), original);
    }
    std::fs::write(&out, b"existing recipe").unwrap();
    let edit = project_bundle::RecipeEdit::open(&plain).unwrap();
    let checks = std::cell::Cell::new(0);
    assert_eq!(
        edit.save(&out, project, true, || {
            checks.set(checks.get() + 1);
            checks.get() == 3
        })
        .unwrap_err(),
        "Project update cancelled"
    );
    assert_eq!(std::fs::read(out).unwrap(), b"existing recipe");
}
