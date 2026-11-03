//! Writes the `web/packages/rust-client` generator input to the given path.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    let [_, path] = args.as_slice() else {
        return Err("Usage: export-rust-client PATH".into());
    };
    std::fs::write(path, pp_api::client_contract::document_bytes()?)?;
    Ok(())
}
