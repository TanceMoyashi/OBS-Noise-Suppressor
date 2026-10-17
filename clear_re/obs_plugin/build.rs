//! ビルドスクリプト: OBS のインポートライブラリ (vendor/obs.lib) をリンクする。
//!
//! obs.lib は `tools/gen_obs_lib.ps1` がインストール済み OBS の obs.dll の
//! エクスポートテーブルから生成する。OBS 本体のバージョンと一致した
//! シンボルだけを持つため、ヘッダや SDK のダウンロードは不要。

use std::path::PathBuf;

fn main() {
    let vendor = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("vendor");

    println!("cargo:rustc-link-search=native={}", vendor.display());
    println!("cargo:rustc-link-lib=dylib=obs");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=vendor/obs.lib");

    assert!(
        vendor.join("obs.lib").exists(),
        "vendor/obs.lib がありません。tools/gen_obs_lib.ps1 を実行してください"
    );
}
