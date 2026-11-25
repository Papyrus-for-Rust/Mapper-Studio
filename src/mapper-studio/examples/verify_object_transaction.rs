//! Verify deferred decoration activation with real source caches and existing rails.
use std::{
    path::Path,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};
fn main() -> Result<(), String> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() != 7 {
        return Err("verify_object_transaction PROJECT RUST_ROOT CATALOG BINDINGS WORKER NEW_BUNDLE DECOR_SHA".into());
    }
    let destination = Path::new(&args[5]);
    if destination.exists() {
        return Err("Verification destination must not exist".into());
    }
    let mut before = mapper_studio::Project::load(Path::new(&args[0]))?;
    before.decoration = None;
    before.objects = None;
    before.validate()?;
    if before.rails.is_none() {
        return Err("Use an existing rail recipe".into());
    }
    let root = Path::new(&args[1]);
    let catalog = Arc::new(mapper_studio::prefab_assets::load(
        root,
        Path::new(&args[2]),
        before.installation.as_ref().ok_or("Installation missing")?,
        || false,
    )?);
    let bindings = Arc::new(mapper_studio::prefab_textures::Bindings::load(
        root,
        Path::new(&args[3]),
        &catalog,
        || false,
    )?);
    let decoration = mapper_studio::decor_source::Recipe {
        engine: mapper_studio::decor_source::COLLIDER_VERSION.into(),
        source_sha256: args[6].to_str().ok_or("Invalid decoration digest")?.into(),
    };
    let inputs = mapper_studio::object_preparation::Inputs::with_decoration(
        &before,
        catalog,
        bindings,
        decoration.clone(),
    )
    .ok_or("Deferred decoration inputs were rejected")?;
    let polling = inputs
        .with_project(&before)
        .ok_or("Deferred context changed")?;
    let mut preparation = mapper_studio::object_preparation::Preparation::default();
    preparation.start(inputs, || {});
    let start = Instant::now();
    let recipe = loop {
        if let Some(event) = preparation.poll(Some(&polling)) {
            match event {
                mapper_studio::object_preparation::Event::Ready(recipe) => break recipe,
                mapper_studio::object_preparation::Event::Failed(error) => return Err(error),
                mapper_studio::object_preparation::Event::Changed => {
                    return Err("Deferred inputs changed".into());
                }
            }
        }
        if start.elapsed() > Duration::from_secs(180) {
            preparation.cancel();
            return Err("Deferred metadata verification deadline exceeded".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut project = before.clone();
    project.decoration = Some(decoration);
    project.objects = Some(recipe);
    project.validate()?;
    if project.rails != before.rails
        || project.targets != before.targets
        || project.seeds != before.seeds
        || project.terrain_key() != before.terrain_key()
    {
        return Err("Deferred activation changed existing generation settings".into());
    }
    project.save(&destination.with_extension("mapper"))?;
    let cancel = AtomicBool::new(false);
    let generated = mapper_studio::headless::generate(
        &project,
        &mapper_studio::headless::Options {
            rust_root: Some(args[1].clone().into()),
            catalog: Some(args[2].clone().into()),
            bindings: Some(args[3].clone().into()),
            worker: Some(args[4].clone().into()),
            skip_composition: false,
        },
        &cancel,
        |label, f| eprintln!("{:.0}% {label}", f * 100.),
    )?
    .ok_or("Generation cancelled")?;
    let maps = generated.composed.as_ref().ok_or("Missing composition")?;
    let objects = maps
        .objects
        .as_ref()
        .ok_or("Deferred activation omitted live objects")?;
    let rails = maps
        .rails
        .as_ref()
        .ok_or("Deferred activation omitted existing rails")?;
    let object_count = objects.instances.len();
    let tracks = rails.tracks.len();
    let manifest = mapper_studio::headless::save_bundle(&project, &generated, destination, &cancel)?;
    println!(
        "{}",
        serde_json::json!({"format":"mapper-deferred-object-generation-verification","deferred_binding_verified":true,"existing_rail_settings_preserved":true,"terrain_key_and_monument_settings_preserved":true,"objects":object_count,"tracks":tracks,"manifest":manifest})
    );
    Ok(())
}
