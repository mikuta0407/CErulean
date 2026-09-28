//! 実行ループ（Go の run.go の RunUntil）。仮想時間の同期は board.rs。

use crate::arm::StopError;
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
    /// TODO(段階1 の高速化): アイドルスキップ（Go の trySkipIdle）。
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
            }
            r?;
        }
        Ok(())
    }
}
