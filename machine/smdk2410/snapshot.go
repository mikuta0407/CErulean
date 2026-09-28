package smdk2410

import (
	"fmt"
	"io"

	"github.com/mikuta0407/cerulean/snapshot"
)

// スナップショットのチャンク構成（順序固定）:
//
//	machine        Machine 自身（命令数・仮想時間の端数・エントリ）
//	cpu            arm.Core
//	mmu            mmu.MMU（CP15 とソフト TLB）
//	ram            bus の RAM 領域の中身
//	board:kbd      SPI1 のキーボード用マイコン（バス外のボード部品）
//	dev:<領域名>   バスに登録した MMIO デバイス（登録順）
//
// MMIO デバイスはバスの登録から列挙する。snapshot.Stateful を実装して
// いないデバイスは、状態を持たないと明示したもの（openBus）以外エラーに
// する。デバイスを追加したときの保存漏れを構造的に防ぐため。

func (m *Machine) StateVersion() uint16 { return 1 }

func (m *Machine) SaveState(e *snapshot.Encoder) {
	e.U64(m.steps)
	e.U32(m.tickAcc)
	e.U32(m.entryPA)
}

func (m *Machine) LoadState(d *snapshot.Decoder) {
	if !d.CheckVersion(1) {
		return
	}
	m.steps = d.U64()
	m.tickAcc = d.U32()
	m.entryPA = d.U32()
}

type chunk struct {
	name string
	s    snapshot.Stateful
}

func (m *Machine) chunks() ([]chunk, error) {
	cs := []chunk{{"machine", m}, {"cpu", m.cpu}, {"mmu", m.mmu}, {"ram", m.bus},
		{"board:kbd", m.kbd}}
	for _, d := range m.bus.MMIODevices() {
		dev := d.Dev
		if t, ok := dev.(timedDev); ok {
			dev = t.dev // 時間同期のラッパ（run.go）の中身を保存する
		}
		switch dev := dev.(type) {
		case snapshot.Stateful:
			cs = append(cs, chunk{"dev:" + d.Name, dev})
		case openBus:
			// 状態なし
		default:
			return nil, fmt.Errorf("smdk2410: device %s (%08X) does not support snapshots", d.Name, d.Base)
		}
	}
	return cs, nil
}

// SaveSnapshot は全状態を w に書く。imageID は元イメージの識別子
// （呼び出し側が決める。CLI はイメージファイルの SHA-256）。
func (m *Machine) SaveSnapshot(w io.Writer, imageID string) error {
	// 溜めた仮想時間をデバイスに渡してから保存する（毎命令 Advance して
	// いた頃と同じ状態・同じバイト列にするため。run.go 参照）。
	m.syncTime()
	cs, err := m.chunks()
	if err != nil {
		return err
	}
	sw, err := snapshot.NewWriter(w, m.Name(), imageID)
	if err != nil {
		return err
	}
	for _, c := range cs {
		if err := sw.Chunk(c.name, c.s); err != nil {
			return err
		}
	}
	return sw.Close()
}

// LoadSnapshot は New 直後のマシンに r の状態を戻し、保存時の imageID を
// 返す（照合は呼び出し側）。LoadImage・Reset は不要（RAM ごと戻るため）。
// 途中で失敗したマシンの状態は不定なので、捨てて作り直すこと。
func (m *Machine) LoadSnapshot(r io.Reader) (imageID string, err error) {
	cs, err := m.chunks()
	if err != nil {
		return "", err
	}
	sr, err := snapshot.NewReader(r)
	if err != nil {
		return "", err
	}
	if sr.Header.Machine != m.Name() {
		return "", fmt.Errorf("smdk2410: snapshot is for machine %q", sr.Header.Machine)
	}
	for _, c := range cs {
		if err := sr.Chunk(c.name, c.s); err != nil {
			return "", err
		}
	}
	if err := sr.Close(); err != nil {
		return "", err
	}
	m.pending = 0
	m.poll = pollCandidate{}
	m.updateDeadline()
	return sr.Header.ImageID, nil
}
