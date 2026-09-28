//! CErulean のネイティブの開発用 CLI（段階1 で run・info・トレース・一致確認の
//! 出力などを実装する。docs/rust-migration-plan.md §9）。

fn main() {
    eprintln!(
        "cerulean (rust) {}: not implemented yet ({} instructions per virtual second)",
        env!("CARGO_PKG_VERSION"),
        cerulean_core::INSTRUCTIONS_PER_SECOND
    );
    std::process::exit(2);
}
