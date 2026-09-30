//! 実行ループ（Go の run.go の RunUntil）。仮想時間の同期は board.rs。

use crate::arm::{POLL_LOOP_LEN, StopError};
use crate::s3c2410::NO_EVENT;

use super::Machine;

impl Machine {
    /// 命令数 steps() が limit に達するか、エラーが起きるまで実行する。
    /// 入力イベントの適用・画面の取得などは、戻ってから（命令境界で）行う。
    /// エラーを起こした命令も 1 命令と数える。1 命令ずつ step した場合と
    /// 全状態が完全に一致する。
    ///
    /// ブロック実行: CPU に「次のデバイスイベントの期限を生む命令まで」または
    /// limit までの命令数を渡し、CPU の内側のループでまとめて実行させる。
    /// ブロックの途中で時間を持つデバイスに触れたときは、CPU の実行済み命令数から
    /// 仮想時間を追いつかせて（catch_up）同期し、期限が早まったら CPU の上限を
    /// 下げる（update_deadline）。
    ///
    /// アイドルスキップ: CPU が副作用のないポーリングループで待っている間
    /// （arm の idle.rs）、次のデバイスイベントの直前か limit まで、命令数と
    /// 仮想時間だけをまとめて進める。これも全状態が一致する。
    pub fn run_until(&mut self, limit: u64) -> Result<(), StopError> {
        while self.sys.board.steps < limit {
            let b = &mut self.sys.board;
            let mut budget = limit - b.steps;
            if b.deadline != NO_EVENT {
                budget = budget.min(b.steps_to_deadline());
            }
            b.in_run = true;
            b.accounted = 0;
            let r = self.cpu.run(&mut self.sys, budget);
            let b = &mut self.sys.board;
            b.catch_up();
            b.in_run = false;
            if b.pending >= b.deadline {
                b.sync_time();
                // 期限のイベント（DMA の区切り）で積んだ転送を、この命令境界で読む。
                let super::Sys { bus, board, .. } = &mut self.sys;
                board.flush_audio(bus);
            }
            r?;
            if self.cpu.take_spin_hint() && self.idle_skip {
                self.try_skip_idle(limit);
            }
        }
        // 区切りでコンパイル待ちを片付ける（batch に満たないまま残さないため）。
        if self.sys.jit.pending() {
            self.sys.jit_compile();
        }
        Ok(())
    }

    /// アイドルスキップの有無を切り替える（既定は有効）。スキップしても状態は
    /// 一致するが、トレースにはスキップした命令が現れない（命令履歴は補う）。
    pub fn set_idle_skip(&mut self, on: bool) {
        self.idle_skip = on;
        self.poll = None;
    }

    /// アイドルスキップで飛ばした命令数の累計（性能計測用）。
    pub fn idle_skipped(&self) -> u64 {
        self.skipped
    }

    /// ポーリングループの先頭に戻った直後に呼ばれる。1 周前と状態が同一
    /// （かつ間がちょうど 1 周分の命令数）なら不動点なので、limit と次の
    /// デバイスイベントの手前まで命令数と仮想時間だけを進める。
    fn try_skip_idle(&mut self, limit: u64) {
        let Some(st) = self.cpu.poll_loop(&mut self.sys) else {
            self.poll = None;
            return;
        };
        let b = &mut self.sys.board;
        let prev = self.poll.replace((b.steps, st));
        if prev != Some((b.steps.wrapping_sub(POLL_LOOP_LEN), st)) {
            return;
        }
        let mut n = limit - b.steps;
        if b.deadline != NO_EVENT {
            // 期限のティックを生む命令は実際に実行する（割り込みを上げる命令
            // 境界を 1 命令ずつの場合と揃えるため）。飛ばしてよいのは、
            // 生むティックが r = deadline-pending-1 以下に収まる命令数まで:
            // (tick_acc + 3k) / 8 <= r。
            let r = b.deadline - b.pending - 1;
            if r < 0 {
                return;
            }
            let kmax = (r as u64)
                .wrapping_mul(8)
                .wrapping_add(7)
                .wrapping_sub(b.tick_acc as u64)
                / super::PCLK_TICKS_NUM;
            n = n.min(kmax);
        }
        n -= n % POLL_LOOP_LEN; // 1 周単位（ループ先頭の状態を保つ）
        if n == 0 {
            return;
        }
        b.add_ticks(n);
        b.steps += n;
        self.skipped += n;
        self.cpu.skip_poll_loop(n);
        self.poll = Some((b.steps, st));
    }
}
