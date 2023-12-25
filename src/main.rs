use anyhow::Result;

use clap::Parser;

use wally_package_types::Command;

fn main() -> Result<()> {
    Command::parse().run()
}
