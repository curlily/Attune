//! Print which music apps MyJam finds on this computer.
//!
//!     cargo run -p jam-core --example detect

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let found = jam_core::detect::detect_providers().await?;
    println!("{}", serde_json::to_string_pretty(&found)?);
    Ok(())
}
