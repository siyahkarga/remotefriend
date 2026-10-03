//! Native internet path test: Hello(ID) -> Accepted -> inner Handshake -> frame.
//! RF_PLAIN_OK=1 ./target/debug/examples/rvtest 127.0.0.1:33202 ID STRONG_PASSWORD
use remote_friend_common::io::{read_packet, read_rv, write_packet, write_rv};
use remote_friend_common::{Handshake, Packet, RvMsg, PROTOCOL_VERSION};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let srv = args.get(1).cloned().unwrap_or("127.0.0.1:33202".into());
    let id: String = args.get(2).cloned().unwrap_or_default().chars().filter(|c| c.is_ascii_digit()).collect();
    let pass = args.get(3).cloned().unwrap_or_default();
    let s = tokio::net::TcpStream::connect(&srv).await.expect("could not connect to srv");
    // wrap in TLS if configured (RF_RV_FP), otherwise plain
    let fp = std::env::var("RF_RV_FP").ok().filter(|s| !s.is_empty());
    let (rd, wr): (Box<dyn tokio::io::AsyncRead + Unpin + Send>, Box<dyn tokio::io::AsyncWrite + Unpin + Send>) = if let Some(fp) = fp {
        remote_friend_common::tls::init_crypto();
        let t = remote_friend_common::tls::tls_connect(&srv, srv.split(':').next().unwrap_or("rv"), Some(fp)).await.expect("TLS");
        let (r, w) = tokio::io::split(t);
        (Box::new(r), Box::new(w))
    } else {
        let (r, w) = s.into_split();
        (Box::new(r), Box::new(w))
    };
    let (mut s_rd, mut s_wr) = (rd, wr);
    macro_rules! wv { ($m:expr) => { write_rv(&mut s_wr, $m).await.unwrap() }; }
    macro_rules! rv { () => { read_rv(&mut s_rd).await.unwrap() }; }
    macro_rules! wp { ($m:expr) => { write_packet(&mut s_wr, $m).await.unwrap() }; }
    macro_rules! rp { () => { read_packet(&mut s_rd).await.unwrap() }; }
    wv!(&RvMsg::Hello { id, auth: Some(pass.clone()) });
    loop {
        match rv!() {
            RvMsg::Accepted => break,
            RvMsg::Rejected(m) => panic!("rejected: {m}"),
            RvMsg::WaitApproval => println!("waiting for approval..."),
            _ => {}
        }
    }
    println!("Accepted, sending inner handshake");
    wp!(&Packet::Handshake(Handshake { version: PROTOCOL_VERSION, password: pass, want_video: true, want_input: true }));
    let mut n = 0;
    loop {
        let pkt = rp!();
        if let Packet::Video(f) = pkt {
            n += 1;
            if n == 1 || n % 20 == 0 {
                println!("frame {n}: seq={} {}x{} {:?} {}b", f.seq, f.width, f.height, f.codec, f.data.len());
            }
            if n >= 40 { break; }
        }
    }
    // also try input
    wp!(&Packet::Input(remote_friend_common::InputEvent::Scroll { dx: 0, dy: 1 }));
    println!("RVTEST OK");
}
