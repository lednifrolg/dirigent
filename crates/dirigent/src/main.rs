//! `dirigent`: terminal client for the IKEA DIRIGERA smart home hub over its local API.

use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "dirigent", version, about, arg_required_else_help = true)]
struct Cli {}

fn main() {
    Cli::parse();
}
