fn main() {
    match xcap::Monitor::all() {
        Ok(mons) => {
            println!("monitor sayisi: {}", mons.len());
            for m in &mons {
                println!(" - {} {}x{}", m.name().unwrap_or("?".into()), m.width().unwrap_or(0), m.height().unwrap_or(0));
            }
            if let Some(m) = mons.into_iter().next() {
                match m.capture_image() {
                    Ok(img) => {
                        println!("CAPTURE OK {}x{}", img.width(), img.height());
                        let _ = img.save("/tmp/rf_capture_test.png");
                        println!("kaydedildi /tmp/rf_capture_test.png");
                    }
                    Err(e) => println!("CAPTURE FAIL: {e:?}"),
                }
            }
        }
        Err(e) => println!("MONITOR FAIL: {e:?}"),
    }
}
