use morons_protocol::ServerMessage;

fn main() {
    let hello = ServerMessage::hello(env!("CARGO_PKG_VERSION"));
    println!("morons-server prepared {hello:?}");
}
