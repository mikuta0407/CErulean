package smdk2410

import (
	"bytes"
	"os"
	"testing"
	"time"

	"github.com/mikuta0407/cerulean/loader"
)

// 実イメージでのスナップショット往復テスト。イメージはリポジトリに
// 含めないので、環境変数 CERULEAN_IMAGE（例 tmp/images/PPC_USA.bin の
// 絶対パス）があるときだけ走る。所要 数十秒。
//
// 「N 命令で保存 → 復元して M 命令」と「通しで N+M 命令」で、UART 出力と
// 最終状態（スナップショットのバイト列）が一致することを確かめる。
func TestRealImageSnapshotResume(t *testing.T) {
	path := os.Getenv("CERULEAN_IMAGE")
	if path == "" {
		t.Skip("CERULEAN_IMAGE not set")
	}
	if testing.Short() {
		t.Skip("slow")
	}
	const n, m = 300_000_000, 100_000_000

	img, err := loader.Load(path, 0x30000000)
	if err != nil {
		t.Fatal(err)
	}
	var outA bytes.Buffer
	a, err := New(&outA)
	if err != nil {
		t.Fatal(err)
	}
	if err := a.LoadImage(img); err != nil {
		t.Fatal(err)
	}
	a.SetRTC(time.Date(2006, 1, 2, 15, 4, 5, 0, time.Local))
	a.Reset()
	run(t, a, n)
	snap := save(t, a)
	atSave := outA.Len()

	var outB bytes.Buffer
	b, err := New(&outB)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := b.LoadSnapshot(bytes.NewReader(snap)); err != nil {
		t.Fatal(err)
	}
	run(t, a, m)
	run(t, b, m)
	if outB.String() != outA.String()[atSave:] {
		t.Errorf("UART output differs after resume:\n got %q\nwant %q", outB.String(), outA.String()[atSave:])
	}
	if !bytes.Equal(save(t, a), save(t, b)) {
		t.Error("final state differs between resumed and continuous runs")
	}
}
