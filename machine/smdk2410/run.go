package smdk2410

import (
	"github.com/mikuta0407/cerulean/bus"
	"github.com/mikuta0407/cerulean/cpu/arm"
	"github.com/mikuta0407/cerulean/device/s3c2410"
)

// 実行ループと仮想時間（性能対策。ユーザー確認済み 2026-09）。
//
// 1. デバイス時間のまとめ進め: 仮想時間の PCLK ティックは毎命令ではなく
//    pending に溜め、次の 2 つの時点でだけデバイスの Advance を呼ぶ。
//      - 溜まったティックが「次のデバイスイベント（タイマー満了・ADC 変換
//        完了）」の期限に達した命令の直後（1 命令ずつ進めた場合と同じ命令境界）
//      - CPU（またはホスト側の入力 API）が時間を持つデバイスに触れる直前
//        （timedDev ラッパ）。レジスタ読み出しが常に最新の時刻を反映する。
//    期限より手前では Advance は加算的（Advance(a)+Advance(b) == Advance(a+b)）
//    なので、全状態が毎命令 Advance した場合と一致する。
// 2. アイドルスキップ: CPU が副作用のないポーリングループで待っている間
//    （cpu/arm の idle.go）、次のデバイスイベントの直前か RunUntil の上限まで、
//    命令数と仮想時間だけをまとめて進める。これも全状態が一致する。
//
// スナップショットは保存前に同期する（pending=0）ので、形式は変わらない。

// RunUntil は命令数 Steps() が limit に達するか、エラーが起きるまで実行する。
// 入力イベントの適用・画面の取得などは、戻ってから（命令境界で）行う。
// エラーの扱いは Step と同じ（エラーを起こした命令も 1 命令と数える）。
func (m *Machine) RunUntil(limit uint64) error {
	for m.steps < limit {
		err := m.cpu.Step()
		m.steps++
		// 仮想時間を進める: 1 命令 = pclkTicksNum/8 PCLK ティック。
		// pclkTicksNum < 8 なので 1 命令で増えるのは高々 1 ティック。
		m.tickAcc += pclkTicksNum
		if m.tickAcc >= 8 {
			m.tickAcc -= 8
			m.pending++
			if m.pending >= m.deadline {
				m.syncTime()
			}
		}
		if err != nil {
			return err
		}
		if m.cpu.TakeSpinHint() && m.idleSkip {
			m.trySkipIdle(limit)
		}
	}
	return nil
}

// Step は 1 命令ぶん進める（machine.Machine）。
func (m *Machine) Step() error { return m.RunUntil(m.steps + 1) }

// SetIdleSkip はアイドルスキップの有無を切り替える（既定は有効）。
// スキップしても状態は一致するが、トレース・命令履歴にはスキップした
// 命令が現れない。
func (m *Machine) SetIdleSkip(on bool) {
	m.idleSkip = on
	m.poll = pollCandidate{}
}

// IdleSkipped はアイドルスキップで飛ばした命令数の累計（性能計測用）。
func (m *Machine) IdleSkipped() uint64 { return m.skipped }

// pollCandidate はポーリングループの先頭で観測した状態（2 周の一致判定用）。
type pollCandidate struct {
	valid bool
	step  uint64 // 観測時の Steps()
	state arm.PollState
}

// trySkipIdle はポーリングループの先頭に戻った直後に呼ばれる。1 周前と
// 状態が同一（かつ間がちょうど 1 周分の命令数）なら不動点なので、limit と
// 次のデバイスイベントの手前まで命令数と仮想時間だけを進める。
func (m *Machine) trySkipIdle(limit uint64) {
	st, ok := m.cpu.PollLoop()
	if !ok {
		m.poll.valid = false
		return
	}
	prev := m.poll
	m.poll = pollCandidate{valid: true, step: m.steps, state: st}
	if !prev.valid || prev.step+arm.PollLoopLen != m.steps || prev.state != st {
		return
	}
	n := limit - m.steps
	if m.deadline != s3c2410.NoEvent {
		// 期限のティックを生む命令は実際に実行する（割り込みを上げる命令
		// 境界を 1 命令ずつの場合と揃えるため）。飛ばしてよいのは、
		// 生むティックが r = deadline-pending-1 以下に収まる命令数まで:
		// (tickAcc + pclkTicksNum*k) / 8 <= r。
		r := m.deadline - m.pending - 1
		if r < 0 {
			return
		}
		kmax := (uint64(r)*8 + 7 - uint64(m.tickAcc)) / pclkTicksNum
		n = min(n, kmax)
	}
	n -= n % arm.PollLoopLen // 1 周単位（ループ先頭の状態を保つ）
	if n == 0 {
		return
	}
	total := uint64(m.tickAcc) + pclkTicksNum*n
	m.pending += int64(total >> 3)
	m.tickAcc = uint32(total & 7)
	m.steps += n
	m.skipped += n
	m.cpu.SkipPollLoop(n)
	m.poll.step = m.steps
}

// syncTime は溜めたティックをデバイスに渡し、次の期限を求め直す。
func (m *Machine) syncTime() {
	if m.pending > 0 {
		t := m.pending
		m.pending = 0
		m.timer.Advance(t)
		m.rtc.Advance(t)
		m.adc.Advance(t)
	}
	m.updateDeadline()
}

// updateDeadline は次のデバイスイベントまでのティック数を求める。
// 時間を持つデバイスの状態が変わったら（レジスタ書き込み・入力）呼ぶ。
func (m *Machine) updateDeadline() {
	m.deadline = min(m.timer.NextEvent(), m.adc.NextEvent())
}

// timedDev は時間を持つデバイス（タイマー・RTC・ADC）の MMIO ラッパ。
// アクセスの前に溜めたティックを渡し、後で期限を求め直す（読み出しで
// 変換が始まる ADC の READ_START のように、読み出しも状態を変え得るため）。
type timedDev struct {
	m   *Machine
	dev bus.Device
}

func (t timedDev) Read(off uint32, size int) uint32 {
	t.m.syncTime()
	v := t.dev.Read(off, size)
	t.m.updateDeadline()
	return v
}

func (t timedDev) Write(off uint32, size int, v uint32) {
	t.m.syncTime()
	t.dev.Write(off, size, v)
	t.m.updateDeadline()
}
