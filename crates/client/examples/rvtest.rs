//! Native internet yolu testi: Hello(ID) -> Accepted -> iç Handshake -> frame.
//! RF_PLAIN_OK=1 ./target/debug/examples/rvtest 127.0.0.1:33202 ID 1234
use remote_friend_common::io::{read_packet, read_rv, write_packet, write_rv};
use remote_friend_common::{Handshake, Packet, RvMsg, PROTOCOL_VERSION};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let srv = args.get(1).cloned().unwrap_or("127.0.0.1:33202".into());
    let id: String = args.get(2).cloned().unwrap_or_default().chars().filter(|c| c.is_ascii_digit()).collect();
    let pass = args.get(3).cloned().unwrap_or("1234".into());
    let mut s = tokio::net::TcpStream::connect(&srv).await.expect("srv bağlanamadı");
    write_rv(&mut s, &RvMsg::Hello { id }).await.unwrap();
    loop {
        match read_rv(&mut s).await.unwrap() {
            RvMsg::Accepted => break,
            RvMsg::Rejected(m) => panic!("reddedildi: {m}"),
            RvMsg::WaitApproval => println!("onay bekleniyor..."),
            _ => {}
        }
    }
    println!("Accepted, iç handshake gönderiliyor");
    write_packet(&mut s, &Packet::Handshake(Handshake { version: PROTOCOL_VERSION, password: pass, want_video: true, want_input: true })).await.unwrap();
    let mut n = 0;
    loop {
        let pkt = read_packet(&mut s).await.unwrap();
        if let Packet::Video(f) = pkt {
            n += 1;
            if n == 1 || n % 20 == 0 {
                println!("frame {n}: seq={} {}x{} {:?} {}b", f.seq, f.width, f.height, f.codec, f.data.len());
            }
            if n >= 40 { break; }
        }
    }
    // input da dene
    write_packet(&mut s, &Packet::Input(remote_friend_common::InputEvent::Scroll { dx: 0, dy: 1 })).await.unwrap();
    println!("RVTEST OK");
}
