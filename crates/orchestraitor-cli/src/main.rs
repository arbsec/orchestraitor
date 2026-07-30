//! `orc` command-line binary.

#![forbid(unsafe_code)]

fn main() -> miette::Result<()> {
    orchestraitor_cli::run()
}
