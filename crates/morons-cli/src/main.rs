use std::error::Error;

use morons_cli::connect_or_start;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    connect_or_start().await?;
    println!("connected to morons-server");
    Ok(())
}
