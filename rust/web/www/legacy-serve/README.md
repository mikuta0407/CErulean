# Go 版 serve の UI（参考用）

Go 版（2026-09-28 に退役。最後のコミットは 039ccb4）の `cerulean serve` が配信して
いたブラウザ UI。Go の HTTP API（`GET /frame` のロングポーリング・`POST /input`・
`POST /control`・`POST /record/*`）を前提にしているので、このままでは動かない。

段階3 のブラウザ版（Worker ＋ wasm）で、PC のキー → キー名の対応表（F1〜F5 →
App1〜5 など）と、表示座標 → 240×320 の変換をここから移す（計画書 §7.1）。
移し終えたら消す。
