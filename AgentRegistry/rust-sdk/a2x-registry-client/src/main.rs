// SPDX-FileCopyrightText: 2026 Daniel Thompson-Yvetot and CrabNebula
// SPDX-License-Identifier: Apache-2.0

//! `a2x-registry-client` binary entry point.

use clap::Parser;

fn main() {
    let cli = a2x_registry_client::cli::Cli::parse();
    if let Err(e) = cli.logging.init() {
        eprintln!("{e}");
    }
    cli.env.apply_or_exit(ap_support::env::EnvPolicy::base());
    std::process::exit(a2x_registry_client::cli::run(cli));
}
