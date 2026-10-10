//! `orc-codegraph` binary: `index <project-dir>` builds the index snapshot;
//! `serve <project-dir>` runs the read-only MCP server over stdio.

use std::io::Write as _;
use std::path::PathBuf;

use orchestraitor_codegraph::{CodegraphServer, persist};

fn main() {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| usage());
    let project = PathBuf::from(args.next().unwrap_or_else(|| usage()));
    match command.as_str() {
        "index" => {
            let mut indexer = orchestraitor_context::Indexer::default();
            let report = indexer
                .index_repository(&project)
                .unwrap_or_else(|error| die(&format!("indexing failed: {error}")));
            persist::store_index(&project, indexer.index())
                .unwrap_or_else(|error| die(&format!("index store failed: {error}")));
            let report_line = format!(
                "indexed {} blobs -> {}",
                report.observed_blobs,
                project.join(persist::INDEX_FILE).display()
            );
            let stderr = std::io::stderr();
            let _ignore = writeln!(stderr.lock(), "{report_line}");
        }
        "serve" => {
            let server =
                CodegraphServer::load(&project).unwrap_or_else(|error| die(&error.to_string()));
            orchestraitor_codegraph::serve_stdio(server)
                .unwrap_or_else(|error| die(&error.to_string()));
        }
        other => die(&format!(
            "unknown command `{other}`; usage: orc-codegraph <index|serve> <project-dir>"
        )),
    }
}

fn usage() -> ! {
    die("usage: orc-codegraph <index|serve> <project-dir>")
}

fn die(message: &str) -> ! {
    let stderr = std::io::stderr();
    let _ignore = writeln!(stderr.lock(), "orc-codegraph: {message}");
    std::process::exit(1);
}
