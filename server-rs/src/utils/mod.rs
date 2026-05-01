// Utilities for image processing, background tasks, etc.

pub fn ensure_directories() -> anyhow::Result<()> {
    // Ensure data/images exist for local image cache
    std::fs::create_dir_all("data/images")?;
    Ok(())
}
