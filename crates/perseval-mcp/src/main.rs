#[tokio::main]
async fn main() {
    let result = perseval_mcp::ipc::run_stdio_entrypoint().await;
    if let Err(error) = result {
        eprintln!("perseval-mcp: {error}");
        std::process::exit(1);
    }
}
