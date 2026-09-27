# CErulean 設計方針

Windows Mobile 5.0（WinCE 5.0）LLE エミュレータ。Go 製。最終的に gomobile bind で iOS/Android に載せる。

## 絶対条件

- **コアは純 Go**。cgo 禁止。UI・OS 依存コードをコア（cmd 以外の全パッケージ）に入れない。
- **CPU はインタプリタ**。iOS では JIT 不可のため。ただし命令デコードと実行を分離してあり、
  将来デコードキャッシュ・ブロック単位実行を追加できる構造を保つこと。
- **既存エミュレータのコードをコピー・参照しない**。実装の根拠は一次資料のみ:
  - ARM Architecture Reference Manual（DDI 0100、ARMv4T/v5TE）
  - Samsung S3C2410 データシート
  - Windows CE 5.0 のドキュメント（BIN 形式、OEMAddressTable 等）
- **仕様が不確かな箇所は推測で埋めない**。`TODO:` コメントで疑問点を残し、ユーザーに質問する。
- 設計判断には短い理由コメントを残す（ユーザーは Go は読めるがエミュレータ開発は初めて）。

## パッケージ境界

```
cmd/cerulean → machine/smdk2410 → { cpu/arm, mmu, bus, device/s3c2410, loader }
                                     cpu/arm → cpu (interface), mmu → bus
```

- `cpu` パッケージは **interface のみ**。コア実装（`cpu/arm`）は将来 Rust 等に差し替えられるよう、
  この境界を跨ぐ依存を作らない。
- **S3C2410 固有の知識（アドレスマップ、レジスタ配置）は `device/s3c2410` と `machine/smdk2410` だけが持つ**。
  cpu・bus・mmu に SoC 固有の定数を入れない。machine は差し替え可能（将来 PXA27x 構成を追加する）。
- ロード時の CE 仮想アドレス→物理アドレス変換（OEMAddressTable 相当）は machine の責務。

## メモリアクセスの設計

- `cpu.Memory` interface（error 返し）を経由する。error はアボート相当で、
  当面はエミュレーション停止、将来はデータアボート例外に変換する。
- リトルエンディアン固定（WinCE/ARM は LE）。
- 性能面: interface 経由アクセスは遅いが、まず正しさ優先。
  高速化（RAM 直アクセス fast path、デコードキャッシュ）は後のマイルストーン。

## 例外・エラーの扱い

- 未実装命令は `arm.UndefinedError{PC, Word, ...}` を返して停止し、CLI が PC と命令語を表示する。
  黙って NOP にしない（デバッグ不能になるため）。
- 未マップアドレスへのアクセスは `bus.BusError`（アドレス付き）。

## テスト方針

- テーブル駆動。フラグ計算（NZCV、シフタキャリー）を重点的に。
- ローダーのテストは合成バイナリで書く（実イメージをリポジトリに入れない）。

## 実イメージについて確認済みの事実（2026-09 検証）

- WM5 Pocket PC SDK（archive.org の `windows-mobile-5.0-pocket-pc-sdk_202305`）内の
  `PPC_USA.bin`（21MB、`tmp/images/` に抽出済み）は **B000FF（BIN）形式**。
- イメージ範囲: start=0x80070000 length=0x01421ED0、エントリ=0x80076CF0、レコード 99 個
  （チェックサム全数一致）。
- **BIN の終端レコードは `{addr=0, len=エントリポイント, checksum=0}`**（addr が 0 で終端）。
- VA 0x80000000 → PA 0x30000000 の変換で、実イメージのブートコードが実行できることを確認済み
  （CP15 キャッシュ/TLB 操作 → GPIO 0x56000050 への書き込みまで到達）。

## 未確定事項・次の課題（随時更新）

- 実行は GPIO コントローラ（0x56000000 台）へのアクセスで停止する。
  次のマイルストーンで GPIO・クロック(0x4C000000)・メモリコントローラ(0x48000000)・
  割り込み(0x4A000000)・タイマー等のスタブが必要。
- MMU の変換テーブルウォーク（WinCE カーネルは起動早々に MMU を有効化するはず）。
  有効化されると現状は明示的にエラーで停止する（mmu/mmu.go）。
- Thumb 命令セット、乗算命令（未実装。当たると UndefinedError で停止する）。
- 日本語版イメージ: 「Localized Windows Mobile 5.0 Pocket PC Emulator Images」
  （JPN 版 msi）の入手先が未発見。archive.org には USA 版 SDK のみ確認。
- リセット時の初期状態の仮定（SVC モード、IRQ/FIQ 禁止、MMU off、ARM state）は
  実イメージのブートコードが素直に走っているので当面問題なし。
