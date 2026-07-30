//! `orc` command-line binary.

#![forbid(unsafe_code)]

fn main() -> std::process::ExitCode {
    match orchestraitor_cli::run() {
        Ok(code) => code.into(),
        Err(error) => {
            let stderr = std::io::stderr();
            let mut lock = stderr.lock();
            let _ = std::io::Write::write_fmt(&mut lock, format_args!("{error:?}\n"));
            error.exit_code().into()
        }
    }
}
