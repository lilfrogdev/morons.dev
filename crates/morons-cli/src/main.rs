use morons_protocol::ClientMessage;

fn main() {
    let hello = ClientMessage::hello(env!("CARGO_PKG_VERSION"));
    println!("morons CLI prepared {hello:?}");
}
