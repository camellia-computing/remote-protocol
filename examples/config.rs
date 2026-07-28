extern crate camellia_remote_protocol;

fn main() {
    println!(
        "{:?}",
        camellia_remote_protocol::config::PeerConfig::load("455058072")
    );
}
