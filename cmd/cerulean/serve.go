package main

import (
	"bytes"
	"context"
	"embed"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io/fs"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/signal"
	"path/filepath"
	"strconv"
	"sync"
	"time"

	"github.com/mikuta0407/cerulean/emu"
	"github.com/mikuta0407/cerulean/machine/smdk2410"
	"github.com/mikuta0407/cerulean/script"
)

// serve: ブラウザ型の対話フロントエンド（ユーザー確認済み 2026-09）。
//
//   - エミュレーションは 1 つの goroutine（loop）がマシンを専有して進める。
//     HTTP ハンドラはマシンに直接触れず、クロージャを cmds に送って loop に
//     実行してもらう（マシンは goroutine 安全でないため）。
//   - 入力は loop が次に止まった命令境界で emu.Session.Inject する。記録は
//     その命令数つきなので、書き出したスクリプトで決定論的に再生できる。
//   - 実時間との同期はここ（フロントエンド）の責務: 仮想時間 sliceSec ずつ
//     進め、壁時計より進みすぎたら待つ。maxLag 以上遅れたら追いつくのを
//     諦めて基準を取り直す（遅れを溜めて後で早回しにしない）。
//   - 画面は GET /frame?since=N のロングポーリング。前回（N）から変わって
//     いれば RGBA の生データを返し、変わらなければ最大 1 秒待って 204。

//go:embed web
var webFS embed.FS

const (
	sliceSec     = 0.01 // 1 回の Run で進める仮想時間（秒）
	maxLag       = 100 * time.Millisecond
	frameEvery   = 15 * time.Millisecond // 画面を取り込む最短間隔（壁時計）
	pollTimeout  = time.Second
	statusWindow = time.Second // 実時間比の計測窓
)

func cmdServe(args []string) {
	fs := flag.NewFlagSet("serve", flag.ExitOnError)
	addr := fs.String("addr", "127.0.0.1:8080", "待ち受けアドレス")
	snapLoad := fs.String("snap-load", "", "スナップショットから再開")
	rtcFlag := fs.String("rtc", "", "RTC 初期時刻 (YYYY-MM-DDTHH:MM:SS、既定は現在時刻)")
	dir := fs.String("dir", ".", "スナップショット・記録の保存先ディレクトリ")
	nb0Base := fs.Uint64("nb0-base", defaultNB0Base, ".nb0 のロード先アドレス")
	noIdleSkip := fs.Bool("no-idle-skip", false, "アイドルループのスキップを無効にする")
	_ = fs.Parse(args)
	if fs.NArg() > 1 || (fs.NArg() == 0 && *snapLoad == "") {
		usage()
	}
	if *snapLoad != "" && *rtcFlag != "" {
		fatal(errors.New("-rtc cannot be used with -snap-load (the RTC state comes from the snapshot)"))
	}
	if host, _, err := net.SplitHostPort(*addr); err != nil {
		fatal(fmt.Errorf("-addr: %w", err))
	} else if ip := net.ParseIP(host); host != "localhost" && (ip == nil || !ip.IsLoopback()) {
		// 認証が無いので、既定ではローカル以外に公開しない。
		fmt.Fprintf(os.Stderr, "cerulean: warning: listening on a non-loopback address %s (no authentication)\n", *addr)
	}
	if err := os.MkdirAll(*dir, 0o755); err != nil {
		fatal(err)
	}

	m, err := smdk2410.New(os.Stdout)
	if err != nil {
		fatal(err)
	}
	imageID, err := startMachine(m, fs.Arg(0), *snapLoad, *rtcFlag, uint32(*nb0Base))
	if err != nil {
		fatal(err)
	}
	m.SetIdleSkip(!*noIdleSkip)

	srv := newServer(m, imageID, *dir)
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt)
	defer cancel()
	go srv.loop(ctx)

	ln, err := net.Listen("tcp", *addr)
	if err != nil {
		fatal(err)
	}
	hs := &http.Server{Handler: srv.handler()}
	go func() {
		<-ctx.Done()
		sctx, c := context.WithTimeout(context.Background(), 2*time.Second)
		defer c()
		_ = hs.Shutdown(sctx)
	}()
	fmt.Fprintf(os.Stderr, "cerulean: serving on http://%s/ (Ctrl-C to stop)\n", ln.Addr())
	if err := hs.Serve(ln); err != nil && !errors.Is(err, http.ErrServerClosed) {
		fatal(err)
	}
}

// server は serve の状態。「loop 専用」の欄は loop の goroutine（と、
// loop が cmds から実行するクロージャ）だけが触る。
type server struct {
	m       *smdk2410.Machine
	sess    *emu.Session
	imageID string
	dir     string
	cmds    chan func()

	// loop 専用
	paused     bool
	speed      float64   // 実時間の何倍で進めるか。0 = 最高速（待たない）
	rebase     bool      // 壁時計との対応を取り直す
	runErr     error     // エミュレーションが止まった原因
	recSnap    string    // 記録中なら起点のスナップショット
	lastPix    []byte    // 最後に公開した画面（変化の判定用）
	winStart   time.Time // 実時間比の計測窓の始点
	winSteps   uint64
	winSkipped uint64

	// ハンドラと共有（mu で保護）
	mu       sync.Mutex
	frame    []byte // RGBA
	frameW   int
	frameH   int
	frameSeq uint64
	frameCh  chan struct{} // 画面が変わったら close して作り直す
	status   serveStatus
}

type serveStatus struct {
	Steps       uint64  `json:"steps"`
	VirtualSec  float64 `json:"virtualSec"`
	Ratio       float64 `json:"ratio"`       // 直近の実時間比（仮想秒/実秒）
	MIPS        float64 `json:"mips"`        // 直近の命令/秒（百万、アイドルスキップ込み）
	IdleSkipped float64 `json:"idleSkipped"` // 直近でアイドルスキップした命令の割合
	Paused      bool    `json:"paused"`
	Speed       float64 `json:"speed"`
	Recording   bool    `json:"recording"`
	Error       string  `json:"error,omitempty"`
}

func newServer(m *smdk2410.Machine, imageID, dir string) *server {
	s := &server{
		m: m, sess: emu.New(m), imageID: imageID, dir: dir,
		cmds: make(chan func()), speed: 1, frameCh: make(chan struct{}),
	}
	s.publishFrame()
	s.winStart, s.winSteps, s.winSkipped = time.Now(), m.Steps(), m.IdleSkipped()
	s.publishStatus(0, 0, 0)
	return s
}

// do は fn を loop の goroutine で実行し、結果を待つ。
func (s *server) do(ctx context.Context, fn func() (any, error)) (any, error) {
	type result struct {
		v   any
		err error
	}
	ch := make(chan result, 1)
	select {
	case s.cmds <- func() { v, err := fn(); ch <- result{v, err} }:
	case <-ctx.Done():
		return nil, ctx.Err()
	}
	select {
	case r := <-ch:
		return r.v, r.err
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}

// loop はエミュレーションを進める（server の型コメント参照）。
func (s *server) loop(ctx context.Context) {
	ips := float64(s.m.InstructionsPerSecond())
	slice := uint64(ips * sliceSec)
	t0, s0 := time.Now(), s.m.Steps()
	var lastFrame time.Time
	for {
		// 溜まった依頼を先に処理する（入力はこの命令境界で入る）。
	drain:
		for {
			select {
			case f := <-s.cmds:
				f()
			default:
				break drain
			}
		}
		if s.rebase {
			t0, s0, s.rebase = time.Now(), s.m.Steps(), false
		}
		if s.paused || s.runErr != nil {
			s.publishFrame()
			s.publishStatus(0, 0, 0)
			select {
			case f := <-s.cmds:
				f()
				s.rebase = true
			case <-ctx.Done():
				return
			}
			continue
		}
		quit, err := s.sess.Run(s.m.Steps() + slice)
		if err == nil && quit {
			err = errors.New("script quit") // serve では予定イベントを使わないので起きない
		}
		if err != nil {
			s.runErr = err
			fmt.Fprintf(os.Stderr, "cerulean: emulation stopped at step %d, PC=%08X: %v\n", s.m.Steps(), s.m.CPU().PC(), err)
		}
		now := time.Now()
		if now.Sub(lastFrame) >= frameEvery {
			s.publishFrame()
			lastFrame = now
		}
		if d := now.Sub(s.winStart); d >= statusWindow {
			n := s.m.Steps() - s.winSteps
			skipped := float64(s.m.IdleSkipped()-s.winSkipped) / float64(max(n, 1))
			s.publishStatus(float64(n)/ips/d.Seconds(), float64(n)/d.Seconds()/1e6, skipped)
			s.winStart, s.winSteps, s.winSkipped = now, s.m.Steps(), s.m.IdleSkipped()
		}
		if s.speed == 0 {
			continue
		}
		want := t0.Add(time.Duration(float64(s.m.Steps()-s0) / ips / s.speed * float64(time.Second)))
		if d := want.Sub(now); d > 0 {
			t := time.NewTimer(d)
			select {
			case f := <-s.cmds:
				f()
			case <-t.C:
			case <-ctx.Done():
				t.Stop()
				return
			}
			t.Stop()
		} else if -d > maxLag {
			s.rebase = true // 遅れすぎ: 追いつくのを諦める
		}
	}
}

// publishFrame は画面が変わっていれば共有領域に写して待ち手を起こす（loop 専用）。
func (s *server) publishFrame() {
	img, err := s.m.Frame()
	if err != nil {
		return // 表示無効（ブート直後など）: 前の画面のまま
	}
	if bytes.Equal(img.Pix, s.lastPix) {
		return
	}
	s.lastPix = bytes.Clone(img.Pix)
	s.mu.Lock()
	s.frame = s.lastPix
	s.frameW, s.frameH = img.Rect.Dx(), img.Rect.Dy()
	s.frameSeq++
	close(s.frameCh)
	s.frameCh = make(chan struct{})
	s.mu.Unlock()
}

// publishStatus は状態表示を更新する（loop 専用）。
// ratio・mips・skipped が 0 なら（計測窓の途中の更新）直前の値を保つ。
func (s *server) publishStatus(ratio, mips, skipped float64) {
	st := serveStatus{
		Steps:      s.m.Steps(),
		VirtualSec: float64(s.m.Steps()) / float64(s.m.InstructionsPerSecond()),
		Ratio:      ratio, MIPS: mips, IdleSkipped: skipped,
		Paused: s.paused, Speed: s.speed, Recording: s.sess.Recording(),
	}
	if s.runErr != nil {
		st.Error = s.runErr.Error()
	}
	s.mu.Lock()
	if st.Ratio == 0 && st.MIPS == 0 && !st.Paused && st.Error == "" {
		st.Ratio, st.MIPS, st.IdleSkipped = s.status.Ratio, s.status.MIPS, s.status.IdleSkipped
	}
	s.status = st
	s.mu.Unlock()
}

func (s *server) handler() http.Handler {
	mux := http.NewServeMux()
	web, _ := fs.Sub(webFS, "web")
	mux.Handle("GET /", http.FileServerFS(web))
	mux.HandleFunc("GET /frame", s.handleFrame)
	mux.HandleFunc("GET /status", func(w http.ResponseWriter, r *http.Request) {
		s.mu.Lock()
		st := s.status
		s.mu.Unlock()
		writeJSON(w, st)
	})
	mux.HandleFunc("POST /input", s.post(s.handleInput))
	mux.HandleFunc("POST /control", s.post(s.handleControl))
	mux.HandleFunc("POST /snapshot", s.post(s.handleSnapshot))
	mux.HandleFunc("POST /record/start", s.post(s.handleRecordStart))
	mux.HandleFunc("POST /record/stop", s.post(s.handleRecordStop))
	return mux
}

// handleFrame: ?since=N より新しい画面があれば返す。無ければ最大 pollTimeout
// 待ち、それでも無ければ 204。ヘッダ X-Frame-Seq/X-Frame-Width/X-Frame-Height。
func (s *server) handleFrame(w http.ResponseWriter, r *http.Request) {
	since, _ := strconv.ParseUint(r.URL.Query().Get("since"), 10, 64)
	timer := time.NewTimer(pollTimeout)
	defer timer.Stop()
	for {
		s.mu.Lock()
		seq, pix, fw, fh, ch := s.frameSeq, s.frame, s.frameW, s.frameH, s.frameCh
		s.mu.Unlock()
		if seq > since && pix != nil {
			h := w.Header()
			h.Set("Content-Type", "application/octet-stream")
			h.Set("Cache-Control", "no-store")
			h.Set("X-Frame-Seq", strconv.FormatUint(seq, 10))
			h.Set("X-Frame-Width", strconv.Itoa(fw))
			h.Set("X-Frame-Height", strconv.Itoa(fh))
			_, _ = w.Write(pix) // pix は公開後に書き換えない（毎回新しく複製する）
			return
		}
		select {
		case <-ch:
		case <-timer.C:
			w.WriteHeader(http.StatusNoContent)
			return
		case <-r.Context().Done():
			return
		}
	}
}

// post は POST ハンドラの共通処理: 別サイトのページからの要求（CSRF）を
// 断り、JSON を返す。
func (s *server) post(h func(r *http.Request) (any, error)) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if o := r.Header.Get("Origin"); o != "" {
			if u, err := url.Parse(o); err != nil || u.Host != r.Host {
				http.Error(w, "cross-origin request refused", http.StatusForbidden)
				return
			}
		}
		v, err := h(r)
		if err != nil {
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusBadRequest)
			_ = json.NewEncoder(w).Encode(map[string]string{"error": err.Error()})
			return
		}
		writeJSON(w, v)
	}
}

func writeJSON(w http.ResponseWriter, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Cache-Control", "no-store")
	_ = json.NewEncoder(w).Encode(v)
}

// inputReq は POST /input の本文。t: down|move|up|keydown|keyup。
type inputReq struct {
	T   string `json:"t"`
	X   int    `json:"x"`
	Y   int    `json:"y"`
	Key string `json:"key"`
}

func (s *server) handleInput(r *http.Request) (any, error) {
	var req inputReq
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		return nil, err
	}
	ev := script.Event{X: req.X, Y: req.Y, Key: req.Key}
	switch req.T {
	case "down":
		ev.Kind = script.TouchDown
	case "move":
		ev.Kind = script.TouchMove
	case "up":
		ev.Kind = script.TouchUp
	case "keydown":
		ev.Kind = script.KeyDown
	case "keyup":
		ev.Kind = script.KeyUp
	default:
		return nil, fmt.Errorf("unknown input type %q", req.T)
	}
	return s.do(r.Context(), func() (any, error) {
		if err := s.sess.Inject(ev); err != nil {
			return nil, err
		}
		return map[string]uint64{"step": s.m.Steps()}, nil
	})
}

// controlReq は POST /control の本文。op: pause|resume|speed（speed は 0 = 最高速）。
type controlReq struct {
	Op    string  `json:"op"`
	Speed float64 `json:"speed"`
}

func (s *server) handleControl(r *http.Request) (any, error) {
	var req controlReq
	if err := json.NewDecoder(r.Body).Decode(&req); err != nil {
		return nil, err
	}
	return s.do(r.Context(), func() (any, error) {
		switch req.Op {
		case "pause":
			s.paused = true
		case "resume":
			s.paused = false
		case "speed":
			if req.Speed < 0 || req.Speed > 100 {
				return nil, fmt.Errorf("speed %v out of range", req.Speed)
			}
			s.speed = req.Speed
		default:
			return nil, fmt.Errorf("unknown op %q", req.Op)
		}
		s.rebase = true
		s.publishStatus(0, 0, 0)
		return map[string]any{"paused": s.paused, "speed": s.speed}, nil
	})
}

func (s *server) handleSnapshot(r *http.Request) (any, error) {
	return s.do(r.Context(), func() (any, error) {
		path := filepath.Join(s.dir, fmt.Sprintf("cerulean-%012d.snap", s.m.Steps()))
		if err := saveSnapshot(s.m, path, s.imageID); err != nil {
			return nil, err
		}
		return map[string]any{"path": path, "step": s.m.Steps()}, nil
	})
}

// handleRecordStart は起点のスナップショットを保存して記録を始める
// （再生には同じ命令境界の状態が要るため、両方を同じ loop の手番で行う）。
func (s *server) handleRecordStart(r *http.Request) (any, error) {
	return s.do(r.Context(), func() (any, error) {
		if s.sess.Recording() {
			return nil, errors.New("already recording")
		}
		path := filepath.Join(s.dir, fmt.Sprintf("rec-%012d.snap", s.m.Steps()))
		if err := saveSnapshot(s.m, path, s.imageID); err != nil {
			return nil, err
		}
		s.recSnap = path
		s.sess.StartRecording()
		s.publishStatus(0, 0, 0)
		return map[string]any{"snapshot": path, "step": s.m.Steps()}, nil
	})
}

// recordResult は POST /record/stop の結果。Script を run -snap-load Snapshot
// で再生すると、最後に Replay へ画面を書く。Expected（停止時の画面）と
// 同一になるはず。
type recordResult struct {
	Snapshot string `json:"snapshot"`
	Script   string `json:"script"`
	Expected string `json:"expected"`
	Replay   string `json:"replay"`
	Events   int    `json:"events"`
	Text     string `json:"text"`
	Command  string `json:"command"`
}

func (s *server) handleRecordStop(r *http.Request) (any, error) {
	return s.do(r.Context(), func() (any, error) {
		if !s.sess.Recording() {
			return nil, errors.New("not recording")
		}
		start, events := s.sess.StopRecording()
		s.publishStatus(0, 0, 0)
		base := filepath.Join(s.dir, fmt.Sprintf("rec-%012d", start))
		res := recordResult{
			Snapshot: s.recSnap, Script: base + ".script",
			Expected: base + "-expected.png", Replay: base + "-replay.png", Events: len(events),
		}
		// 停止時点の画面を期待値として保存し、再生側も同じ命令境界で画面を
		// 書いて終了するようにする。
		end := s.m.Steps()
		events = append(events,
			script.Event{Step: end, Kind: script.Shot, Path: res.Replay},
			script.Event{Step: end, Kind: script.Quit})
		if err := writeFramebuffer(s.m, res.Expected); err != nil {
			return nil, err
		}
		var text bytes.Buffer
		if err := emu.Format(&text, s.recSnap, s.imageID, start, events); err != nil {
			return nil, err
		}
		if err := os.WriteFile(res.Script, text.Bytes(), 0o644); err != nil {
			return nil, err
		}
		res.Text = text.String()
		res.Command = fmt.Sprintf("cerulean run -snap-load %s -script %s", res.Snapshot, res.Script)
		fmt.Fprintf(os.Stderr, "cerulean: recorded %d events to %s\n", len(events)-2, res.Script)
		return res, nil
	})
}
