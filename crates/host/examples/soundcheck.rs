//! Records the computer's sound for a few seconds and prints what arrived.
//! Run: cargo run -p remote-friend-host --example soundcheck

fn main() {
    tracing_subscriber::fmt().with_max_level(tracing::Level::INFO).init();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let mut rx = remote_friend_host::audio::subscribe();
        let end = tokio::time::Instant::now() + std::time::Duration::from_secs(4);
        let (mut n, mut bytes) = (0u32, 0usize);
        while let Ok(Ok(p)) = tokio::time::timeout_at(end, rx.recv()).await {
            n += 1;
            bytes += p.opus.len();
            if n <= 3 {
                println!("packet {}: {} bytes opus, {} bytes pcm", p.seq, p.opus.len(), p.pcm24.len());
            }
        }
        println!("{n} packets in 4 s ({} kbit/s)", bytes * 8 / 4000);
    });
}
