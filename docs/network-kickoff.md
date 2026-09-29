# インターネット接続（中継サーバー経由）の開始用プロンプト

新しいセッションの最初に、以下の区切り線から下をそのまま貼り付けて使う。

---

CErulean（WM5 LLE エミュレータ、Rust 製）の WM5 を **インターネットに繋ぎたい**。ブラウザ版では
中継サーバー経由で外に出る形にし、**メニューのトグルでオン・オフを切り替えられる** ように
する（既定はオフ）。設計方針・Rust の規則・確認済みの事実は CLAUDE.md、計画は
docs/rust-migration-plan.md（特に §7.5 の「サイトはネットワーク通信をしない」の方針）、
ブラウザ版の現状は README と rust/web/www/app を読んでから始めること。

## 分かっていること（2026-09-29 の調査）

- ROM にネットワークのモジュールがある: `ne2000.dll`（NE2000 互換の NIC のドライバ）・`ndis.dll`・
  `tcpstk.dll`・`tcpip6.dll`・`dhcp.dll`・`ws2.dll`・`wininet.dll`・`netui.dll` 等。
- ブート中に NE2000 を探すアクセスは見えなかった（PA 0x00000000〜0x2FFFFFFF を監視）。
  代わりに `pcc_smdk2410.dll` が PC カードコントローラ（Intel 82365SL 互換。PA 0x110003E0/
  0x110003E1 のインデックス／データのポート）を探して、見つからずに諦めている。
  NE2000 は PC カードとして挿されたときに読み込まれる形と推測している（要確認。レジストリの
  PCMCIA の設定・ne2000 の読み込み条件を観察で確かめる）。
- **PC カードコントローラはストレージカードの作業で実装済み**（2026-09-29、コミット 1b50a2c。
  設計・観察・根拠は docs/storage-card-design.md）。NE2000 はそのコントローラに挿す別のカードとして
  足せばよい:
  - `rust/core/src/pccard/pd6710.rs` の `Card` トレイト（属性・共通メモリ・I/O の読み書き、
    RDY/-IREQ、-IOIS16、RESET、電源）を NE2000 のカードで実装する。ボード（smdk2410/board.rs）は
    今 `Option<CfCard>` を持っているので、カードの種類を持てる形（enum）にする。
  - 配線は確定済み: I/O は PA 0x11000000+ポート、メモリは PA 0x10000000+ISA アドレス、
    管理割り込み（-INTR）→ EINT3、カードの IRQ3 → EINT8（IRQ3 は atadisk が選んだ値からの判断。
    NE2000 が別の IRQ を選んだら配線の判断を見直す）。
  - 挿抜はスクリプトの `card insert/eject` と同じ形の入力（emu の Kind・Session::record_applied）。
    スナップショットは machine の版数 2 の pcic・cf チャンクの後に足す（版数を上げ、旧版を読む）。
- **レジストリの PCMCIA の検出の表**（`Drivers\PCMCIA\Detect`。起動後の RAM から読んだ）:
  10 serial.dll DetectModem・50 atadisk.dll DetectATADisk・**60 NE2000.DLL DetectNE2000**
  （`NE2000$`、Prefix NDS、Miniport NE2000）・99 SRAMDisk。カードが挿されると順に検出の関数が
  呼ばれ、最初に名乗り出たドライバが読み込まれる。atadisk は CIS の CISTPL_FUNCE で判断して
  いた。DetectNE2000 が CIS の何を見るか（FUNCID=6 のネットワーク？ 製造者 ID？）はまだ見ていない
  → 最初の観察の対象。CISTPL_CFTABLE_ENTRY のどの構成を選ぶかも atadisk と同じ手順で追う。
- 別の経路として Device Emulator の ActiveSync（DMA トランスポート `dmatrans.dll`・
  `serdma.dll`）経由の接続もあるが、パソコン側の ActiveSync まで作ることになり範囲が大きい。
- Device Emulator のソースは「既存エミュレータのコード」なので参照しない。

## 構成の案（最初にユーザーと決める）

- ゲスト側: 82365SL 互換のコントローラ＋NE2000 の PC カード（CIS と DP8390 相当の NIC。
  一次資料は National Semiconductor DP8390D のデータシート、PC Card Standard）。受信
  バッファ・送信・割り込みを仮想時間で扱う。**受信は外から来るので、入力イベントとして
  記録する**（受信したフレームと命令数。記録を再生すれば同じ状態になる決定論を保つ）。
- ホスト側（ブラウザ）: 生の TCP/UDP は使えない（fetch・WebSocket・WebTransport のみ）。
  - 案 A: WebSocket でイーサネットのフレームをそのまま中継するサーバー（サーバー側で
    TAP＋NAT、または slirp 相当のユーザー空間の NAT）。ブラウザ側は簡単。
  - 案 B: ブラウザ内に DHCP・ARP・DNS と TCP の終端（ユーザー空間の NAT）を持ち、外へは
    TCP のストリームだけを WebSocket で中継する（サーバーが単純になる）。
  - 中継サーバーは本リポジトリに小さなものを置く（言語・依存はユーザーと決める。CLI の
    ネイティブ版は直接ソケットを使える）。
- UI: メニューに「ネットワーク（中継サーバー経由）」のトグルと中継サーバーの URL。オンにした
  ときだけ接続する（オフなら今どおりサイトは外と通信しない）。オン・オフの切り替えは
  カードの挿入・抜去（またはリンクの状態）としてゲストに見せ、記録する。
- 計画書 §7.5 の方針（サイトはネットワーク通信をしない）を「オンにしたときだけ、ユーザーが
  指定した中継サーバーとだけ通信する」に改め、README・画面に明記する。

## 約束（CLAUDE.md・計画書が正）

- 既存エミュレータのコードは参照しない。仕様が不確かな箇所は推測で埋めず TODO を残して聞く。
- 既定の構成（ネットワークなし）では今の基準と一致させる。コアの動作が変わるなら
  `tools/golden/regen.sh` で作り直し、理由をコミットに残す。状態を足したらスナップショットの
  版数を上げる（旧版の読み込みを残す）。
- 決定論: ネットワークから来たものはすべて命令数つきの入力として記録する。
- 依存の追加（WebSocket のクレート・中継サーバーの依存）はユーザーの承認を取る。
- セキュリティ: 中継サーバーは開いたプロキシにならないよう、既定は localhost だけで待ち受け、
  公開するなら認証を付ける案を出す。
- コミット前に `tools/check.sh`、`CERULEAN_IMAGE=tmp/images/PPC_USA.bin tools/golden/verify.sh`、
  ブラウザ版は `node tools/browser/app-e2e.mjs tmp/images/PPC_USA.bin tmp/app-e2e`。

## 調べ方のメモ

- 調査用の道具（`tools/re/`。いずれも Python の標準ライブラリだけ）:
  - `romtoc.py <image> [modules|files]`: ROM のモジュール一覧（vbase・大きさ・名前）とファイル
    一覧。例: `tools/re/romtoc.py tmp/images/PPC_USA.bin > tmp/mods.txt`（ne2000.dll は
    VA 0x01560000〜0x9000、pcc_serv.dll 0x01550000、pcmcia.dll 0x01520000、
    pcc_smdk2410.dll 0x01740000、ceddk.dll 0x03DD0000）。
  - `pcsample.py <mods.txt> [最小回数]`: `run --sample N --no-idle-skip` の出力をモジュール名の
    時系列にする（プロセスのスロットに移った PC も引ける）。「挿した後、どの DLL が動いて
    どこで止まったか」を見るのに使った。
  - `pcicwatch.py <log>`: `--watch 0x10000000-0x17FFFFFF` のログ（標準エラー）をコントローラの
    レジスタ操作（reg[XX]=値）と CIS の読み出しに並べ直す。
- ドライバの中を追う: 気になる区間の直前でスナップショットを取り（`--snap-save F@Ni`。
  max-steps は N+1 以上）、`--trace --trace-from N` の出力から PC が目的の DLL の範囲
  （PC の下位 25 ビットで比べる）のものだけを抜き出す。ドライバの呼び出しは
  `ldr r12,[pc,#4]; ldr r12,[r12]; bx r12` の間接呼び出しを挟むので、それを除くと読みやすい。
- レジストリの値: ROM のハイブ（default.hv 等）は圧縮されているが、起動後は RAM にある。
  スナップショット（ram チャンクは無圧縮）を UTF-16LE の文字列で検索すると、キー名と値が
  並んで見える（Detect の表はこれで読んだ）。
- 一次資料の PDF のテキスト化: pypdf の wheel を PyPI から取って `tmp/pylib` に展開し
  （この環境には pip がない）、`PYTHONPATH=tmp/pylib python3 -c 'import pypdf …'`
  （`tmp/pdf2txt.sh in.pdf out.txt` がそれ。非コミット）。PD6710 のデータシートは
  pcmcia-cs.sourceforge.net/specs/ にあり、同じ場所に他のコントローラや NIC の資料もある。
- **Today のスナップショットは今のビルドで作り直すこと**: コントローラを足す前のもの
  （machine の版数 1）はドライバがプローブに失敗した状態なので、カードを挿しても検出されない。
  `cerulean run --rtc 2006-01-02T15:04:05 --snap-save tmp/today.snap@3700000000i
  --max-steps 3700000001 tmp/images/PPC_USA.bin`（約 40 秒）。
- 挿した直後から File Explorer 等を操作するスクリプトの例（座標は USA 版）: Start (12,8) →
  Programs (50,171) → File Explorer (196,110) → Up (43,306) ×2 → Storage Card (60,147)。
  JPN 版（`tmp/images/PPC_JPN.bin`）も同じ手順で起動し、ファイル エクスプローラは (36,190)。
- Today のスナップショットの作り方、スクリプトでの操作は docs/storage-card-kickoff.md の
  「調べ方のメモ」も参照。
- WM5 側の確認は Pocket Internet Explorer（Start メニューの Internet Explorer）で
  http のページを開く。
