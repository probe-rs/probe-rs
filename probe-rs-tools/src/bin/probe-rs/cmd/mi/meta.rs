pub fn run() -> anyhow::Result<()> {
    let meta = crate::util::meta::current_meta()?;
    let meta = serde_json::to_string(&meta)?;
    println!("{meta}");
    Ok(())
}
