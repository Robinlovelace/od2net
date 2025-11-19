use anyhow::Result;
use clap::Parser;
use od2net::{run_pipeline, CliArgs};

fn main() -> Result<()> {
    let args = CliArgs::parse();
    run_pipeline(args)
}
