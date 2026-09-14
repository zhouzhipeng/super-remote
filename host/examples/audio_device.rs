fn main() -> anyhow::Result<()> {
    wasapi::initialize_mta().ok()?;
    let e = wasapi::DeviceEnumerator::new()?;
    let d = e.get_default_device(&wasapi::Direction::Render)?;
    println!(
        "{}",
        serde_json::json!({"endpoint": d.get_id()?, "name": d.get_friendlyname()?})
    );
    Ok(())
}
