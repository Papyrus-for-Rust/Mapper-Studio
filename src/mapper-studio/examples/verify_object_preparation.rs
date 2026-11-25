//! Exercise the workbench's asynchronous source-discovery path without opening a window.
use mapper_studio::{
    Project,
    object_preparation::{Event, Inputs, Preparation},
    prefab_assets, prefab_textures,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().map(PathBuf::from).collect();
    if args.len() != 5 {
        return Err("usage: verify_object_preparation BOUND_PROJECT ROOT CATALOG BINDINGS".into());
    }
    let mut project = Project::load(&args[1])?;
    let expected = project
        .objects
        .take()
        .ok_or("Missing expected object binding")?;
    let original = project.clone();
    let catalog = Arc::new(prefab_assets::load(
        &args[2],
        &args[3],
        project
            .installation
            .as_ref()
            .ok_or("Missing installation")?,
        || false,
    )?);
    let bindings = Arc::new(prefab_textures::Bindings::load(
        &args[2],
        &args[4],
        &catalog,
        || false,
    )?);
    let inputs = Inputs::new(&project, catalog.clone(), bindings.clone())
        .ok_or("Missing preparation inputs")?;
    let checks = Inputs::new(&project, catalog, bindings).unwrap();
    let notifications = Arc::new(AtomicUsize::new(0));
    let notification = notifications.clone();
    let mut worker = Preparation::default();
    worker.start(inputs, move || {
        notification.fetch_add(1, Ordering::SeqCst);
    });
    let started = Instant::now();
    loop {
        if let Some(event) = worker.poll(Some(&checks)) {
            match event {
                Event::Ready(recipe) => {
                    assert_eq!(recipe, expected);
                    assert_eq!(project, original);
                    assert!(project.objects.is_none());
                    project.objects = Some(recipe);
                    project.validate()?;
                    println!(
                        "{}",
                        serde_json::json!({"format":"papyrus-workbench-object-preparation-verification","source_sha256":expected.road_source_sha256,"preserved_project_until_ready":true,"binding_matches_explicit_cli_recipe":true,"gui_opened":false})
                    );
                }
                Event::Failed(error) => return Err(error.into()),
                Event::Changed => return Err("Unexpected source change".into()),
            }
            break;
        }
        if started.elapsed() > Duration::from_secs(180) {
            worker.cancel();
            return Err("Preparation exceeded verification deadline".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}
