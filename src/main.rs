//! webgrab エントリポイント（設計§4 main）。本文=stdout、診断=stderr、終了コード変換。

use clap::Parser;
use std::process::ExitCode as ProcExit;
use webgrab::cli::Cli;
use webgrab::error::ExitCode;
use webgrab::netguard;
use webgrab::pipeline;

#[tokio::main]
async fn main() -> ProcExit {
    let cli = Cli::parse();

    // URLスキームの早期検証（clap通過後）
    match url::Url::parse(&cli.url) {
        Ok(u) if netguard::is_allowed_scheme(u.scheme()) => {}
        Ok(u) => {
            eprintln!(
                "webgrab: error=usage unsupported scheme: {} (only http/https)",
                u.scheme()
            );
            return ProcExit::from(ExitCode::Usage as u8);
        }
        Err(e) => {
            eprintln!("webgrab: error=usage invalid URL");
            eprintln!("{e}");
            return ProcExit::from(ExitCode::Usage as u8);
        }
    }

    match pipeline::run(&cli).await {
        Ok(out) => {
            if let Some(path) = &cli.output {
                if let Err(e) = tokio::fs::write(path, out.as_bytes()).await {
                    eprintln!("webgrab: error=internal output write failed");
                    eprintln!("{e}");
                    return ProcExit::from(ExitCode::Internal as u8);
                }
            } else {
                println!("{out}");
            }
            ProcExit::SUCCESS
        }
        Err(e) => {
            e.print_stderr();
            ProcExit::from(e.code as u8)
        }
    }
}
