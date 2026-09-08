// Synthetic helper only: never opens a browser or touches the clipboard.
use std::{
    env,
    io::{self, Write},
    process, thread,
    time::Duration,
};

fn main() {
    match env::args().nth(1).as_deref() {
        Some("ok") => {}
        Some("reject") => process::exit(1),
        Some("hold") => thread::sleep(Duration::from_secs(30)),
        Some(kind @ ("copy" | "bad_ack")) => {
            let mut output = io::stdout().lock();
            output
                .write_all(&[if kind == "copy" { 1 } else { b'x' }])
                .unwrap();
            output.flush().unwrap();
            io::copy(&mut io::stdin().lock(), &mut io::sink()).unwrap();
        }
        _ => process::exit(2),
    }
}
