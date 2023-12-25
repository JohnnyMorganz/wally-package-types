use log::{error, Level, LevelFilter};
use std::io::Write;

use clap::Parser;

use console::style;
use wally_package_types::Command;

fn main() {
    env_logger::Builder::from_env("LOG")
        .filter_level(LevelFilter::Info)
        .format(move |buf, record| {
            let tag = match record.level() {
                Level::Error => style("error").red(),
                Level::Warn => style("warn").yellow(),
                Level::Info => style("info").green(),
                Level::Debug => style("debug").cyan(),
                Level::Trace => style("trace").magenta(),
            }
            .bold();

            writeln!(buf, "{}{} {}", tag, style(":").bold(), record.args())
        })
        .init();

    let exit_code = match Command::parse().run() {
        Ok(_) => 0,
        Err(err) => {
            error!("{:#}", err);
            1
        }
    };

    std::process::exit(exit_code)
}
