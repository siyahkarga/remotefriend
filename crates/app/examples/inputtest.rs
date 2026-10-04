//! Synthetic input test: send Scroll + Key + MouseMove, read 3 frames.
//! cargo run -p remote-friend-client --example inputtest -- 127.0.0.1:33200 STRONG_PASSWORD
use remote_friend_common::{Handshake, InputEvent, MouseButton, Packet, RemoteKey, PROTOCOL_VERSION};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let host = args.get(1).cloned().unwrap_or("127.0.0.1:33200".into());
    let pass = args.get(2).cloned().unwrap_or_default();
    let mut s = tokio::net::TcpStream::connect(&host).await.expect("could not connect");
    let hs = Packet::Handshake(Handshake { version: PROTOCOL_VERSION, password: pass, want_video: true, want_input: true });
    write(&mut s, &hs).await;
    println!("response: {:?}", read(&mut s).await.unwrap());

    let tests = [
        Packet::Input(InputEvent::MouseMove { x: 100, y: 100 }),
        Packet::Input(InputEvent::MouseDown { button: MouseButton::Left }),
        Packet::Input(InputEvent::MouseUp { button: MouseButton::Left }),
        Packet::Input(InputEvent::Scroll { dx: 0, dy: 3 }),
        Packet::Input(InputEvent::Scroll { dx: 0, dy: -3 }),
        Packet::Input(InputEvent::Key { key: RemoteKey::Char('a'), down: true }),
        Packet::Input(InputEvent::Key { key: RemoteKey::Char('a'), down: false }),
        Packet::Input(InputEvent::Key { key: RemoteKey::Up, down: true }),
        Packet::Input(InputEvent::Key { key: RemoteKey::Up, down: false }),
        Packet::Input(InputEvent::Key { key: RemoteKey::Shift, down: true }),
        Packet::Input(InputEvent::Key { key: RemoteKey::F5, down: true }),
        Packet::Input(InputEvent::Key { key: RemoteKey::F5, down: false }),
        Packet::Input(InputEvent::Key { key: RemoteKey::Shift, down: false }),
    ];
    for p in &tests {
        write(&mut s, p).await;
    }
    println!("{} input packets sent", tests.len());
    for i in 0..3 {
        match read(&mut s).await {
            Ok(Packet::Video(f)) => println!("frame {i}: seq={} {}x{} {:?}", f.seq, f.width, f.height, f.codec),
            Ok(o) => println!("frame {i}: {o:?}"),
            Err(e) => println!("frame {i} error: {e:#}"),
        }
    }
    println!("INPUTTEST OK");
}

async fn write(s: &mut tokio::net::TcpStream, p: &Packet) {
    let b = remote_friend_common::encode(p).unwrap();
    s.write_u32(b.len() as u32).await.unwrap();
    s.write_all(&b).await.unwrap();
}

async fn read(s: &mut tokio::net::TcpStream) -> anyhow::Result<Packet> {
    let len = s.read_u32().await? as usize;
    let mut buf = vec![0u8; len];
    s.read_exact(&mut buf).await?;
    Ok(remote_friend_common::decode(&buf)?)
}
