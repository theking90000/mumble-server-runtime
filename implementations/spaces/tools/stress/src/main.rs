use anyhow::Result;
use clap::Parser;
use mumble_spaces_stress::{Command, run};

#[tokio::main]
async fn main() -> Result<()> {
    run(Command::parse()).await
}
