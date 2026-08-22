use morons_protocol::Hello;

fn main() {
    let hello = Hello::current();
    println!("morons-server protocol v{}", hello.protocol_version);
}
