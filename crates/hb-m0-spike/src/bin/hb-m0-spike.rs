//! M0 spike runner — prints a per-leg go/no-go report.
//!
//!   cargo run -p hb-m0-spike                         # legs 1-3 (offline)
//!   HB_M0_RELAY=ws://127.0.0.1:7777 cargo run -p hb-m0-spike   # + leg 4 (relay)
//!
//! The authoritative gate is `cargo test -p hb-m0-spike`; this binary is the readable demo.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("# Hoardbook M0 foundation spike\n");

    println!("leg 1  nostr identity     : {}", hb_m0_spike::identity::demo()?);
    println!("leg 2  npub->iroh binding : {}", hb_m0_spike::binding::demo()?);
    println!("leg 3  NIP-44 listing     : {}", hb_m0_spike::listing::demo()?);

    match std::env::var("HB_M0_RELAY") {
        Ok(url) if !url.is_empty() => {
            let p = hb_m0_spike::relay::run(&url).await?;
            println!(
                "leg 4  strfry relay       : {} · fetched {} · binding_ok={} · listing_ok={}",
                p.relay, p.fetched, p.binding_ok, p.listing_ok
            );
            anyhow::ensure!(
                p.binding_ok && p.listing_ok,
                "relay leg did not round-trip both kinds"
            );
        }
        _ => println!("leg 4  strfry relay       : SKIPPED (set HB_M0_RELAY=ws://host:port)"),
    }

    println!("\nall run legs green.");
    Ok(())
}
