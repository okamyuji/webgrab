//! webgrab エントリポイント（設計§4 main）。本文=stdout、診断=stderr、終了コード変換。

use clap::Parser;
use std::process::ExitCode as ProcExit;
use webgrab::cli::Cli;
use webgrab::error::ExitCode;
use webgrab::netguard;
use webgrab::pipeline;

fn main() -> ProcExit {
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("webgrab: error=internal runtime start failed");
            eprintln!("{e}");
            return ProcExit::from(ExitCode::Internal as u8);
        }
    };
    let code = rt.block_on(run());
    // 期限で打ち切った名前解決（getaddrinfo）はblockingスレッドで動き続け、止める手段が無い。
    // ランタイムを普通に落とすとその終了を待ち、--timeoutを超えてプロセスが残る。
    rt.shutdown_background();
    code
}

async fn run() -> ProcExit {
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
