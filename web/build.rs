// JIT（段階5-2）が生成した関数を本体の関数テーブルに足せるよう、wasm のリンク時に
// テーブルを伸ばせるようにする（既定では lld が最大 = 初期の大きさにする）。
fn main() {
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        println!("cargo:rustc-cdylib-link-arg=--growable-table");
    }
}
