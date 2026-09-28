// Package mmu は ARM920T の MMU（CP15 System Control Coprocessor）を実装する。
// 仕様の根拠は ARM Architecture Reference Manual (DDI 0100) 第B3章。
//
// 設計（ユーザー確認済み 2026-09）:
//   - TLB はエミュレートせず、アクセス毎に毎回テーブルウォークする。
//     正しさ優先で、ウォークが常に最新のテーブルを見るため CP15 の
//     TLB 操作（c8）は no-op でよい。速度が問題になったらソフト TLB を足す。
//   - キャッシュ（c7、C/W/I ビット）はエミュレートしない（書き込み保持のみ）。
//   - MMU 起因のフォルトは cpu.AbortError で返し、CPU が ARM 例外に変換する。
//     ページテーブル自体が未マップ物理を指した場合（bus.BusError）は
//     エミュレータ側の不備なので、そのままエラーを返して停止させる。
package mmu

import (
	"fmt"

	"github.com/mikuta0407/cerulean/cpu"
)

// 制御レジスタ（CP15 c1）の関心ビット。他のビット（C/W/P/D/L/B/I など）は
// 保持するだけで動作に影響しない（キャッシュ非エミュレートのため）。
const (
	ctrlM = 1 << 0  // MMU 有効
	ctrlA = 1 << 1  // アライメントチェック（TODO: 未実装。下記参照）
	ctrlS = 1 << 8  // System 保護
	ctrlR = 1 << 9  // ROM 保護
	ctrlV = 1 << 13 // 例外ベクタを 0xFFFF0000 に
)

// フォルトステータス符号（ARM ARM B3-19 Table）。FSR[3:0] に入る。
const (
	fsAlign      = 0x1
	fsTransSect  = 0x5 // セクション変換フォルト
	fsTransPage  = 0x7
	fsDomainSect = 0x9
	fsDomainPage = 0xB
	fsPermSect   = 0xD
	fsPermPage   = 0xF
)

// MMU は CPU とバスの間に入る仮想→物理変換層。cpu.Memory と
// arm.Coprocessor（CP15）の両方を実装する。
type MMU struct {
	phys cpu.Memory // 物理アドレス空間（bus）

	ctrl uint32 // c1: 制御
	ttb  uint32 // c2: 変換テーブルベース（bits 31:14）
	dacr uint32 // c3: ドメインアクセス制御
	fsr  uint32 // c5: フォルトステータス（データアボートのみ更新）
	far  uint32 // c6: フォルトアドレス
	pid  uint32 // c13: FCSE PID（bits 31:25）。VA<32MB を MVA へ再配置する

	// priv は CPU が特権モードか（arm.Coprocessor.SetPrivileged で更新）。
	// AP ビットの権限チェックに使う。リセット直後は SVC なので true。
	priv bool

	// パイプライン近似: 実機では c1 の M ビットを変えた MCR の時点で
	// 後続 2 命令がフェッチ済みで、WinCE のブートコードは MMU 有効化の
	// 直後の命令が物理アドレスのまま実行されることに依存している。
	// M ビットが変化したら fetchGrace=2 とし、MCR に続く「連続した」命令
	// フェッチをその回数まで変更前の制御レジスタ（prevCtrl）で変換する。
	// 分岐するとパイプラインはフラッシュされる（分岐先は新状態でフェッチ
	// される）ため、非連続アドレスのフェッチで猶予は打ち切る。
	// データアクセスは即時に新しい状態を使う。
	// TODO: 猶予中に割り込み・例外が入るとベクタフェッチで打ち切られる
	// 挙動になるが、この 2 命令は割り込み禁止で走るのが前提のコード。
	fetchGrace int
	graceNext  uint32 // 次に猶予が適用される連続アドレス（0 = 最初の1回は無条件）
	prevCtrl   uint32

	// その他の crn への書き込みを保持する（読み返し用）。
	regs [16]uint32
}

var _ cpu.Memory = (*MMU)(nil)

func New(phys cpu.Memory) *MMU {
	return &MMU{phys: phys, priv: true}
}

// Enabled は MMU（CP15 c1 の M ビット）が有効化されているか。
func (m *MMU) Enabled() bool { return m.ctrl&ctrlM != 0 }

// ---- 変換テーブルウォーク（ARM ARM B3.4）----

// 一次/二次記述子の下位 2 ビット（タイプ）。
const (
	descFault   = 0
	descCoarse  = 1 // 一次: 粗ページテーブル
	descSection = 2 // 一次: セクション
	descFine    = 3 // 一次: 細ページテーブル
	descLarge   = 1 // 二次: 大ページ（64KB）
	descSmall   = 2 // 二次: 小ページ（4KB）
	descTiny    = 3 // 二次: 細ページ（1KB、fine テーブルのみ）
)

// translate は VA→PA 変換と権限チェックを行う。
// フォルトは *cpu.AbortError、テーブル自体が読めない場合は物理バスの
// エラーをそのまま返す（エミュレーション停止）。
func (m *MMU) translate(va uint32, write bool) (uint32, error) {
	return m.translateCtrl(m.ctrl, va, write)
}

// translateCtrl は制御レジスタ値を指定した変換。通常は m.ctrl だが、
// 命令フェッチの猶予期間（Fetch32 参照）だけ変更前の値が渡される。
func (m *MMU) translateCtrl(ctrl, va uint32, write bool) (uint32, error) {
	if ctrl&ctrlM == 0 {
		return va, nil
	}

	// FCSE(c13): VA の下位 32MB は PID で修飾された MVA に再配置される。
	// WinCE はプロセス切替に PID を使う。
	mva := va
	if va < 0x02000000 {
		mva = va | m.pid
	}

	// TODO: アライメントチェック（A ビット）は未実装。現状 CPU 側が
	// アドレスをアラインしてから発行するため、MMU には非アラインの
	// ワード/ハーフワードアクセスが届かない。非アラインアクセスの
	// フォルト化が必要になったら（ユーザーアプリ実行時など）、
	// CPU がマスク前のアドレスを渡す形に変えて対応する。

	// 一次記述子: TTB[31:14] | MVA[31:20] << 2
	l1Addr := (m.ttb &^ 0x3FFF) | (mva>>20)<<2
	l1, err := m.phys.Read32(l1Addr)
	if err != nil {
		return 0, fmt.Errorf("mmu: L1 walk at PA=%08X (VA=%08X): %w", l1Addr, va, err)
	}

	switch l1 & 3 {
	case descSection:
		domain := uint8((l1 >> 5) & 0xF)
		if err := m.checkDomain(domain, va, write, fsDomainSect); err != nil {
			return 0, err
		}
		if m.domainClient(domain) {
			ap := (l1 >> 10) & 3
			if !m.apAllowed(ctrl, ap, write) {
				return 0, &cpu.AbortError{VA: va, Status: fsPermSect, Domain: domain, Write: write}
			}
		}
		return l1&0xFFF00000 | mva&0x000FFFFF, nil

	case descCoarse, descFine:
		domain := uint8((l1 >> 5) & 0xF)
		var l2Addr uint32
		if l1&3 == descCoarse {
			// 粗テーブル: ベース[31:10] | MVA[19:12] << 2（256 エントリ）
			l2Addr = (l1 &^ 0x3FF) | ((mva>>12)&0xFF)<<2
		} else {
			// 細テーブル: ベース[31:12] | MVA[19:10] << 2（1024 エントリ）
			l2Addr = (l1 &^ 0xFFF) | ((mva>>10)&0x3FF)<<2
		}
		l2, err := m.phys.Read32(l2Addr)
		if err != nil {
			return 0, fmt.Errorf("mmu: L2 walk at PA=%08X (VA=%08X): %w", l2Addr, va, err)
		}
		if l2&3 == descFault {
			return 0, &cpu.AbortError{VA: va, Status: fsTransPage, Domain: domain, Write: write}
		}
		if err := m.checkDomain(domain, va, write, fsDomainPage); err != nil {
			return 0, err
		}

		var pa uint32
		var ap uint32
		switch l2 & 3 {
		case descLarge: // 64KB。AP はサブページ（16KB）ごと: MVA[15:14] で選択
			pa = l2&0xFFFF0000 | mva&0x0000FFFF
			ap = (l2 >> (4 + 2*((mva>>14)&3))) & 3
		case descSmall: // 4KB。AP はサブページ（1KB）ごと: MVA[11:10] で選択
			pa = l2&0xFFFFF000 | mva&0x00000FFF
			ap = (l2 >> (4 + 2*((mva>>10)&3))) & 3
		default: // descTiny
			if l1&3 == descCoarse {
				// 粗テーブル内の tiny 記述子は v4 では無効。
				// TODO: ARM ARM では UNPREDICTABLE。変換フォルト扱いにしている。
				return 0, &cpu.AbortError{VA: va, Status: fsTransPage, Domain: domain, Write: write}
			}
			pa = l2&0xFFFFFC00 | mva&0x000003FF // 1KB。AP は 1 個
			ap = (l2 >> 4) & 3
		}
		if m.domainClient(domain) && !m.apAllowed(ctrl, ap, write) {
			return 0, &cpu.AbortError{VA: va, Status: fsPermPage, Domain: domain, Write: write}
		}
		return pa, nil

	default: // descFault
		return 0, &cpu.AbortError{VA: va, Status: fsTransSect, Write: write}
	}
}

// domainClient はドメインがクライアント（AP チェックあり）か。
// マネージャ（11）は権限チェックなしでアクセス可。
func (m *MMU) domainClient(domain uint8) bool {
	return (m.dacr>>(uint(domain)*2))&3 == 1
}

// checkDomain はドメインアクセス制御（DACR）を確認する。
// 00（no access）と 10（予約）はドメインフォルト。
func (m *MMU) checkDomain(domain uint8, va uint32, write bool, status uint8) error {
	switch (m.dacr >> (uint(domain) * 2)) & 3 {
	case 1, 3: // client / manager
		return nil
	default: // 0: no access, 2: 予約（フォルトに倒す）
		return &cpu.AbortError{VA: va, Status: status, Domain: domain, Write: write}
	}
}

// apAllowed は AP ビットと S/R ビットによるアクセス可否（ARM ARM B3-16）。
func (m *MMU) apAllowed(ctrl, ap uint32, write bool) bool {
	switch ap {
	case 0:
		// AP=00 は S/R ビット次第の読み出し専用空間。
		if write {
			return false
		}
		switch {
		case ctrl&ctrlR != 0:
			return true // R=1: 全モード読み出し可
		case ctrl&ctrlS != 0:
			return m.priv // S=1: 特権のみ読み出し可
		default:
			return false
		}
	case 1: // 特権のみ RW
		return m.priv
	case 2: // 特権 RW / ユーザー読み出しのみ
		return m.priv || !write
	default: // 3: 全モード RW
		return true
	}
}

// ---- cpu.Memory ----

func (m *MMU) Read8(a uint32) (uint8, error) {
	pa, err := m.translate(a, false)
	if err != nil {
		return 0, err
	}
	return m.phys.Read8(pa)
}
func (m *MMU) Read16(a uint32) (uint16, error) {
	pa, err := m.translate(a, false)
	if err != nil {
		return 0, err
	}
	return m.phys.Read16(pa)
}
func (m *MMU) Read32(a uint32) (uint32, error) {
	pa, err := m.translate(a, false)
	if err != nil {
		return 0, err
	}
	return m.phys.Read32(pa)
}
func (m *MMU) Write8(a uint32, v uint8) error {
	pa, err := m.translate(a, true)
	if err != nil {
		return err
	}
	return m.phys.Write8(pa, v)
}
func (m *MMU) Write16(a uint32, v uint16) error {
	pa, err := m.translate(a, true)
	if err != nil {
		return err
	}
	return m.phys.Write16(pa, v)
}
func (m *MMU) Write32(a uint32, v uint32) error {
	pa, err := m.translate(a, true)
	if err != nil {
		return err
	}
	return m.phys.Write32(pa, v)
}

// Fetch32 は命令フェッチ（cpu.InstructionFetcher）。M ビット変更直後、
// MCR に続く連続した最大 2 命令だけ変更前の変換状態を使う
// （パイプライン近似。struct コメント参照）。
func (m *MMU) Fetch32(a uint32) (uint32, error) {
	ctrl := m.ctrl
	if m.fetchGrace > 0 {
		if m.graceNext == 0 || a == m.graceNext {
			m.fetchGrace--
			m.graceNext = a + 4
			ctrl = m.prevCtrl
		} else {
			m.fetchGrace = 0 // 分岐した: パイプラインフラッシュ相当
		}
	}
	pa, err := m.translateCtrl(ctrl, a, false)
	if err != nil {
		return 0, err
	}
	return m.phys.Read32(pa)
}

// ---- CP15（arm.Coprocessor を満たす。arm への import は不要）----

// ARM920T の Main ID レジスタ値（ARM920T TRM）。
// 0x41 = ARM Ltd, 920 = part number, rev は適当に 0。
const arm920MainID = 0x41129200

// ARM920T のキャッシュタイプレジスタ（c0, opc2=1）。
// TODO: 0x0D172172 は ARM920T TRM の値のはずだが要再確認（記憶ベース）。
const arm920CacheType = 0x0D172172

func (m *MMU) Read(opc1, crn, crm, opc2 uint8) (uint32, error) {
	switch crn {
	case 0:
		if opc2 == 1 {
			return arm920CacheType, nil
		}
		return arm920MainID, nil
	case 1:
		return m.ctrl, nil
	case 2:
		return m.ttb, nil
	case 3:
		return m.dacr, nil
	case 5:
		return m.fsr, nil
	case 6:
		return m.far, nil
	case 13:
		return m.pid, nil
	}
	return m.regs[crn&15], nil
}

func (m *MMU) Write(opc1, crn, crm, opc2 uint8, v uint32) error {
	switch crn {
	case 1:
		if (m.ctrl^v)&ctrlM != 0 {
			// M ビットが変わる: 後続の連続 2 命令のフェッチは旧状態で行う。
			m.prevCtrl = m.ctrl
			m.fetchGrace = 2
			m.graceNext = 0
		}
		m.ctrl = v
	case 2:
		m.ttb = v
	case 3:
		m.dacr = v
	case 5:
		m.fsr = v // OS がコンテキスト復元で書くことがある
	case 6:
		m.far = v
	case 7:
		// キャッシュ操作（wait-for-interrupt の c7,c0,4 を含む）: no-op。
		// TODO: wait-for-interrupt を「割り込みまで停止」に最適化すると
		// アイドルループが速くなる。当面はビジーループで正しく動く。
	case 8:
		// TLB 操作: 毎回ウォークするので no-op。
	case 13:
		m.pid = v & 0xFE000000
	default:
		m.regs[crn&15] = v
	}
	return nil
}

// VectorBase は例外ベクタのベース（V ビットで 0xFFFF0000 / 0）。
func (m *MMU) VectorBase() uint32 {
	if m.ctrl&ctrlV != 0 {
		return 0xFFFF0000
	}
	return 0
}

// SetPrivileged は CPU の特権状態の通知を受ける（arm.Coprocessor）。
func (m *MMU) SetPrivileged(priv bool) { m.priv = priv }
