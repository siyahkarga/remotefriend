//! Headless test: fetch 5 frames from the host without a GUI and print their sizes.
//! cargo run -p remote-friend-client --example headless -- 127.0.0.1:33200 STRONG_PASSWORD
use remote_friend_common::{Handshake, Packet, PROTOCOL_VERSION};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let host = args.get(1).cloned().unwrap_or("127.0.0.1:33200".into());
    let pass = args.get(2).cloned().unwrap_or_default();
    let mut s = tokio::net::TcpStream::connect(&host).await.expect("could not connect");
    let hs = Packet::Handshake(Handshake { version: PROTOCOL_VERSION, password: pass, want_video: true, want_input: true });
    let b = remote_friend_common::encode(&hs).unwrap();
    s.write_u32(b.len() as u32).await.unwrap();
    s.write_all(&b).await.unwrap();
    let resp = read(&mut s).await.unwrap();
    println!("response: {resp:?}");
    for i in 0..5 {
        match read(&mut s).await {
            Ok(Packet::Video(f)) => {
                println!("frame {i}: seq={} {}x{} codec={:?} {} byte", f.seq, f.width, f.height, f.codec, f.data.len());
                if i == 0 {
                    match f.codec {
                        remote_friend_common::VideoCodec::Jpeg => {
                            std::fs::write("/tmp/rf_win_frame.jpg", &f.data).unwrap();
                            println!("first frame saved: /tmp/rf_win_frame.jpg");
                        }
                        remote_friend_common::VideoCodec::H264 => {
                            use openh264::formats::YUVSource;
                            let mut dec = openh264::decoder::Decoder::new().unwrap();
                            for nal in openh264::nal_units(&f.data) {
                                if let Ok(Some(yuv)) = dec.decode(nal) {
                                    let (w, h) = yuv.dimensions();
                                    let mut rgb = vec![0u8; w * h * 3];
                                    yuv.write_rgb8(&mut rgb);
                                    image::save_buffer("/tmp/rf_h264_test.png", &rgb, w as u32, h as u32, image::ColorType::Rgb8).unwrap();
                                    println!("H264 decode OK {w}x{h}, saved /tmp/rf_h264_test.png");
                                    break;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            Ok(other) => println!("frame {i}: other packet {other:?}"),
            Err(e) => {
                println!("frame {i}: ERROR (expected if the host is on Wayland; works on Xorg/Windows): {e:#}");
                // the host log shows a "capture error"; the connection stays up
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    }
}

async fn read(s: &mut tokio::net::TcpStream) -> anyhow::Result<Packet> {
    let len = s.read_u32().await? as usize;
    let mut buf = vec![0u8; len];
    s.read_exact(&mut buf).await?;
    Ok(remote_friend_common::decode(&buf)?)
}
