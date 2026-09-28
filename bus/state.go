package bus

import "github.com/mikuta0407/cerulean/snapshot"

// スナップショット（snapshot.Stateful）: RAM 領域の中身だけを保存する。
// 領域の配置（regions/pages）は machine の構成（New で決まる）で、
// MMIO デバイスの状態は各デバイスが自分のチャンクで保存する。
// watch はデバッグ用の設定なので保存しない。

var _ snapshot.Stateful = (*Bus)(nil)

func (b *Bus) StateVersion() uint16 { return 1 }

func (b *Bus) SaveState(e *snapshot.Encoder) {
	var n uint64
	for i := range b.regions {
		if b.regions[i].ram != nil {
			n++
		}
	}
	e.U64(n)
	for i := range b.regions {
		r := &b.regions[i]
		if r.ram == nil {
			continue
		}
		e.String(r.name)
		e.Bytes(r.ram)
	}
}

func (b *Bus) LoadState(d *snapshot.Decoder) {
	if !d.CheckVersion(1) {
		return
	}
	var rams []*region
	for i := range b.regions {
		if b.regions[i].ram != nil {
			rams = append(rams, &b.regions[i])
		}
	}
	if n := d.U64(); n != uint64(len(rams)) {
		d.Fail("bus: %d RAM regions in snapshot, machine has %d", n, len(rams))
		return
	}
	// 構成が同じなら領域は登録順に並んでいる。名前で照合して取り違えを防ぐ。
	for _, r := range rams {
		if name := d.String(); name != r.name && d.Err() == nil {
			d.Fail("bus: RAM region %q in snapshot, machine has %q", name, r.name)
			return
		}
		d.BytesInto(r.ram) // 長さ（RAM 量）が違えば Fail
	}
}
