//! Read-only CLI companion to the native folder inspector.
fn main() -> Result<(), String> {
    let root = std::env::args_os()
        .nth(1)
        .ok_or("Usage: inspect_installation <Rust root>")?;
    let source = mapper_studio::installation::inspect(std::path::Path::new(&root), || false)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&source).map_err(|e| e.to_string())?
    );
    Ok(())
}
