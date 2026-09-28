package arm

// execFn は命令の実行関数。word はデコード対象の命令語そのもの。
// フィールド抽出は各 exec 関数内で行う（十分安価なため）。将来ホットに
// なったら、ここを「展開済みフィールドを持つ struct」に変えてキャッシュする。
type execFn func(c *Core, word uint32) error

// Instr はデコード結果。Decode は Core の状態に依存しない純関数なので、
// 結果を物理アドレスをキーにキャッシュしても正しさが保たれる。
type Instr struct {
	Word uint32
	exec execFn
}

// Decode は ARM 命令語 1 個をデコードする。未実装・未定義の命令も
// 「実行するとエラーを返す関数」として返す（デコード自体は失敗しない）。
func Decode(word uint32) Instr {
	return Instr{Word: word, exec: decodeFn(word)}
}

// unimpl は「実行時に UndefinedError を返す」exec 関数を作る。
// PC と命令語は Step が埋める。
func unimpl(reason string) execFn {
	return func(c *Core, word uint32) error {
		return &UndefinedError{Reason: reason}
	}
}

// archUndef は「実機（コプロセッサなし構成）でも未定義例外になる」命令。
// Step がゲストに未定義命令例外として配送する。
func archUndef(reason string) execFn {
	return func(c *Core, word uint32) error {
		return &UndefinedError{Reason: reason, Arch: true}
	}
}

// decodeFn は ARM ARM Figure A3-1 の命令クラス分けに従うディスパッチ。
// 分岐の順序が重要: 「データ処理レジスタ形式」の空間には bit7/bit4 や
// S ビットの組み合わせで乗算・MRS/MSR・BX などが埋め込まれている。
func decodeFn(word uint32) execFn {
	if word>>28 == 0xF {
		// ARMv4 では UNPREDICTABLE、v5 以降は BLX 等の拡張空間。
		// TODO(v5TE): PXA27x 対応時に BLX(1) 等を実装する。
		return unimpl("cond=1111 extension space (ARMv5+)")
	}

	switch (word >> 25) & 7 {
	case 0: // データ処理（レジスタ形式）とその同居命令
		// BX: 0001 0010 1111 1111 1111 0001
		if word&0x0FFFFFF0 == 0x012FFF10 {
			return execBX
		}
		if word&0x90 == 0x90 { // bit7=1 かつ bit4=1: データ処理ではない
			if (word>>5)&3 == 0 {
				// bits[7:4] = 1001: 乗算（MUL/MLA/UMULL...）または SWP
				return execMulSwp
			}
			// bits[7:4] = 1011/1101/1111: ハーフワード・符号付き転送
			return execLdstMisc
		}
		if op := (word >> 21) & 0xF; op >= 8 && op <= 11 && word&(1<<20) == 0 {
			// TST/TEQ/CMP/CMN の S=0 は MRS/MSR の空間
			if op&1 == 0 {
				return execMRS
			}
			return execMSR
		}
		return execDataProc

	case 1: // データ処理（即値形式）
		if op := (word >> 21) & 0xF; op >= 8 && op <= 11 && word&(1<<20) == 0 {
			if op&1 == 1 {
				return execMSR // MSR 即値形式
			}
			return unimpl("undefined (MRS-like encoding with immediate)")
		}
		return execDataProc

	case 2: // LDR/STR 即値オフセット
		return execLdst
	case 3: // LDR/STR レジスタオフセット
		if word&0x10 != 0 {
			// bits[27:25]=011 かつ bit4=1 は ARMv4 のアーキテクチャ未定義空間
			// （実機でも未定義例外）。WinCE はこの空間の命令をトラップとして
			// 意図的に実行するので、例外として配送する。
			return archUndef("architecturally undefined space (011 with bit4)")
		}
		return execLdst
	case 4: // LDM/STM
		return execLdmStm
	case 5: // B/BL
		return execBranch
	case 6: // コプロセッサ LDC/STC: 対応コプロセッサがないので実機同様に未定義例外
		return archUndef("LDC/STC (no coprocessor)")
	default: // 7: コプロセッサ演算・レジスタ転送、SWI
		if word&(1<<24) != 0 {
			return execSWI
		}
		if word&0x10 != 0 {
			return execMcrMrc
		}
		return archUndef("CDP (no coprocessor)")
	}
}
