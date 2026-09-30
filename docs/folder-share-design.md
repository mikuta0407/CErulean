# Device Emulator のフォルダ共有（ネットワークとストレージカードの共存）

2026-09-30。PC カードのソケットが 1 つなので、イーサネットカード（NE2000）と CompactFlash は
同時に挿せない。ROM を変えずに共存させるため、WM5 のイメージに入っている Device Emulator の
フォルダ共有（`emulserv.dll`・`vcefsd.dll`）が使う準仮想デバイスを用意した。WM5 からは
CompactFlash と同じ「Storage Card」に見える。ユーザー了承: ROM は書き換えない、候補 1（本体
ストレージ）の後に候補 2（この共有）を試す。

## 一次資料

ない（Device Emulator 固有の装置で仕様は非公開。Device Emulator のソースは参照しない）。
ゲストのドライバの機械語（`cerulean disasm` でスナップショットから逆アセンブル）だけを根拠に、
ホスト側（この装置）は **ゲストのコードが読む値を、そのコードの使い方と矛盾しないように**
返す。レジスタ・共有バッファ・コマンドの一覧は `rust/core/src/smdk2410/deshare.rs` の先頭。

## 観察（PPC_JPN.bin）

- `emulserv.dll`（VA 0x014B0000〜）は約 4.377 億命令目に PA 0x500F5000（8 バイト）を
  VirtualCopy し、+4 の bit30 を見て +0 に 0xFFFFFFFF を書く。GPIO は GPF3 を EINT3、EXTINT1 の
  EINT11 を High レベルにし、IRQ 39（EINT11）の IST で待つ。GPG3 は EINT11 の機能にしない
  （→ EINTPEND の bit11 を直接立てた。deshare.rs の先頭）。
- IST: +4 の bit30 が立っていれば、PA 0x500F4000（16 バイト）をマップして +4 に 0 を書き
  （確認応答）、+0xC が 1 なら DeactivateDevice、そうでなければレジストリ `Drivers\EMULSERV` の
  DSK（Profile VCEFSD・`EMULATOR SHARED FOLDER FS`）を ActivateDevice する。
- `vcefsd.dll`（VA 0x01590000〜。コードは約 0x2800 バイト）の FSD_MountDisk は共有バッファ
  PA 0x33EFF000（0x42C バイト）・データバッファ PA 0x33EEF000（64KB）をマップし、コマンド 4 の
  結果が 0 なら「Storage Card」を FSDMGR_RegisterVolume し、コマンド 0x15 で 1 回に送れる最大の
  バイト数を得る。
- 送り方（0x015931FC）: +0 にバッファの番地、+4 にコマンド、+8 が 0 になるまで +4 に 0 を書き、
  +0xC を結果としてバッファの +2 に入れる。
- パスの解決（0x01592D1C）は「\」で区切った **先頭からのパス全体**（`\a`、`\a\b` …）を順に
  コマンド 0x11（名前で引く）で確かめ、途中はディレクトリ（属性 0x10）であることを見る。
  ワイルドカードを含む部分は +8 を 0 にして列挙の最初の項目を求める。
- FindFirst・FindNext は応答のバッファを枠（0x638 バイト）に写し、+8（番号）を増やして
  0x11 を送り直し、返った名前をパターンと照合する。FindClose はホストに送らない。
- CloseFile は 0x13（フラッシュ）→ 0x0A（閉じる）の後、変更があれば 0x0E（属性・日時）を
  **閉じたハンドルの番号で** 送る。
- 日時は DOS 形式の 32 ビット（上位が日付）。GetFileTime・SetFileTime・SetFilePointer・
  GetFileSize はゲストの中だけで済む。FlushFileBuffers・縮める SetEndOfFile は非対応
  （ERROR_NOT_SUPPORTED）。

## 実装

- コア: `smdk2410/deshare.rs`（レジスタ・中身のファイルツリー `ShareFs`・コマンド・スナップ
  ショット）。0x500F4000〜0x500F5FFF を専用の装置にし、その外の 0x500F0000 台は今までどおり
  値保持スタブ。コマンドは MMIO の書き込みの直後（次の命令より前）に処理し、RAM は共有バッファと
  データバッファの範囲（0x33EEF000〜0x33EFFFFF）だけを読み書きする（`Devices::after_write`・
  `RamAccess`）。新しい項目の日時はゲストの RTC（決定論的）。
- 中身は装置の中（メモリ）にあり、スナップショットの `deshare` チャンク（machine の版数 4）。
  挿すときにフロントエンドがカードのイメージ（FAT）から作り、抜くとイメージに戻す（属性は
  ディレクトリかどうかだけを保つ）。挿抜は入力として記録する（`share insert <img>`・
  `share eject [img]`）。
- CLI: `run --share IMG`・`--share-out IMG`。ブラウザ版: ネットワークがオン（ソケットに
  イーサネットカード）の間にストレージカードを挿すと、自動でこの方式になる。
- 確認: コアの単体テスト（通知と確認応答・名前で引く・列挙・作成から削除まで・スナップ
  ショット）、CLI で File Explorer の表示・Word Mobile での読み書き・新しいフォルダ、
  `tools/browser/app-net.mjs`（ネットワークと同時に挿して File Explorer に見え、記録を CLI で
  再生して一致）。基準は変わらない（挿さなければ見え方は同じ）。

## 残り・未確認

- +0x500F5000 に書く 0xFFFFFFFF の意味、+8（処理中）を 0 以外にする場合の約束。
- コマンド番号の抜け（0x12・0x14）の意味。
- 属性（読み取り専用・隠し等）は FAT のイメージとの変換で失われる（ツリーの中では保つ）。
- 共有は抜いた瞬間に中身を返すので、抜いた後にゲストが送る閉じる等は失敗として答える。
