use anyhow::Result;

use clap::Parser;

mod command;

use command::Command;

fn main() -> Result<()> {
    Command::parse().run()
}
