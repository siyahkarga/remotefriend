//! Terminal host (alternative to the desktop app): for servers, SSH sessions and scripts.

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    remote_friend_host::init_logging();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("remote-friend-host [--new-password] [--forget-devices]");
        println!("  --new-password    renew the saved password (the old one stops working)");
        println!("  --forget-devices  forget all permanently allowed devices");
        println!();
        println!("Prefer the desktop app (RemoteFriend) unless you need a terminal.");
        return Ok(());
    }
    if args.iter().any(|a| a == "--forget-devices") {
        println!(
            "{} trusted device(s) removed; they will need approval again.",
            remote_friend_host::forget_devices()
        );
        return Ok(());
    }
    remote_friend_host::run(remote_friend_host::RunOptions {
        new_password: args.iter().any(|a| a == "--new-password"),
        terminal: true,
    })
    .await
}
