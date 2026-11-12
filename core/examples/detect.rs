//! Print which music apps Attune finds on this computer.
//!
//!     cargo run -p attune-core --example detect

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let found = attune_core::detect::detect_providers().await?;
    println!("{}", serde_json::to_string_pretty(&found)?);
    Ok(())
}
