//! The music-recommender binary. Wires the tiers together; holds no logic of its own.

fn main() {
    println!("{} {}", env!("CARGO_BIN_NAME"), env!("CARGO_PKG_VERSION"));
}
