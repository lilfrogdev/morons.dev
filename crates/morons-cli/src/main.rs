use morons_protocol::Hello;

fn main() {
    let hello = Hello::current();
    println!("morons CLI protocol v{}", hello.protocol_version);
}
