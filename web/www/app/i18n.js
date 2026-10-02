// UI の文言の切り替え（日本語・英語）。メインスレッドと Worker の両方から使う。
// コードの文言は日本語をそのままキーにして t("…") で引く（英語の辞書に無ければ日本語のまま）。
// 値の差し込みは t("保存 {n}", { n })。index.html の文言は data-i18n="id" で applyDom が差し替える
// （英語のときだけ。日本語は HTML の元の文を戻す）。
const PREF_KEY = "cerulean-lang";

// index.html 用（id → 英語の HTML）
const EN_HTML = {
  recBadge: "● Recording",
  saveBadge: "Saving…",
  pause: "Pause",
  menu: "Menu",
  screenAria: "WM5 screen (tap to operate)",
  profile: "Profile",
  profNew: "New",
  profRename: "Rename",
  profDel: "Delete",
  resume: "Resume",
  pickImage: "Select an image (.bin)",
  loadSnap: "Load a snapshot",
  screenFor: "Screen size when booting from an image",
  optAutoName: "Auto (from the file name)",
  opt240: "240×240 (square)",
  opt320: "320×320 (square; WM6 *_QVGA_VR)",
  opt480: "480×480 (VGA square)",
  language: "Language",
  langAuto: "Auto",
  startNote1: `Saves, storage cards and recordings are separated per profile (images are shared).
        To use environments with different OSes or screen sizes, create a profile and switch to it.
        Switching autosaves the current state and resumes the destination from where it left off.`,
  startNote2: `Select a WM5 / WM6 emulator image (<code>PPC_USA.bin</code>, <code>PPC_JPN.bin</code>, etc.) and it
        fast-forwards to the Today screen and saves automatically on arrival. Choose the screen size to match the image
        ("Auto" uses a size such as 480x640 in the name, otherwise decides by VGA / Square / QVGA). Images and saves
        are used only inside this device and are never sent anywhere.`,
  stopped: "Stopped",
  stopResume: "Resume from the last autosave",
  stopExport: "Export this state",
  stopBack: "Back to the start screen",
  otherTabH: "Running in another tab",
  otherTabP: `Only one tab can run the emulator (so that saves do not conflict).
        Close the other tab and it can run here.`,
  hwButtons: "Hardware buttons",
  up: "Up", left: "Left", ok: "OK", right: "Right", down: "Down",
  secRun: "Run",
  speed: "Speed",
  speed1: "Normal",
  speed2: "2x",
  speedMax: "Maximum",
  skipTurbo: "Stop the boot fast-forward",
  audioOn: "Play sound",
  volume: "Volume",
  audioNote: `Sound plays only at normal speed (not during fast-forward, 2x or maximum speed).
      It breaks up where emulation cannot keep up with real time.`,
  clockSync: "Set the guest clock to now on resume / return",
  clockNow: "Set the clock to now",
  profileColon: "Profile:",
  toStart: "To the start screen (choose profile, image, save)",
  secSave: "Save",
  saveNow: "Save the current state",
  load: "Load",
  saveNote: `Autosave keeps 3 generations (on arriving at Today, when leaving the screen, every 30 seconds if there was input, otherwise every 5 minutes).
      Exported snapshots contain the contents of the image (a Microsoft distribution). Do not share them in public.`,
  secCard: "Storage card",
  cardInsert: "Insert",
  cardEject: "Eject",
  cardExport: "Export image",
  cardImport: "Load image",
  cardNewLabel: "New card",
  cardNew: "Create",
  cardAddFiles: "Add files",
  cardAddDir: "Add a folder",
  cardMkdir: "New folder",
  cardDrag: "You can also drag files here to add them.",
  cardNote: `WM5 sees it as "Storage Card". Adding and removing contents is done here while the card is
      <strong>ejected</strong>. While it is inserted, the contents live inside the emulator and are included in saves (snapshots).
      Ejecting writes them back to this device. While networking is on (while the Ethernet card is using the
      PC card socket), the card is inserted with the Device Emulator's folder-sharing method (WM5 still sees the same "Storage Card").`,
  secNet: "Network (via relay server)",
  netOn: "On (insert the Ethernet card)",
  relayLabel: "Relay server",
  relayPh: "wss://(this site)/relay",
  tokenLabel: "Token",
  netNote1: `Only while this is on, the page talks to the relay server specified here over a WebSocket and has it relay the WM5
      traffic (TCP bytes and destination names). While it is off, this site does not communicate with the outside.
      Start the relay server with <code>cerulean serve --with-relay</code> (the <code>/relay</code> of this site) or
      <code>cerulean relay</code> (the native CLI) and enter the token it shows.
      From an https page you can only connect to a <code>wss://</code> relay (or <code>ws://127.0.0.1</code> on your own device).`,
  netNote2: `<strong>HTTPS</strong>: WM5's IE Mobile only speaks old TLS, so TLS is terminated inside the emulator and the
      relay server reconnects outward with current TLS. Once, first, open <code>http://10.0.2.2/</code> in WM5's IE and
      install the certificate (CErulean Local CA). The key of this CA exists only on this device.`,
  netCaExport: "Export the CA certificate",
  netNote3: `The first time, in WM5 go to "Settings → Connections → Network Card" and set "My network card connects to" to
      "The Internet". The storage card can still be inserted while it is on (using the folder-sharing method).
      Received data is included in the operation recording.`,
  secRec: "Operation recording",
  recStart: "Start recording",
  recScript: "Export the script",
  recSnap: "Export the starting snapshot",
  recCards: "Export the inserted card images",
  recNote: `A recording is a pair of a starting snapshot and an input script with absolute instruction counts.
      Replaying the two exported files with the native CLI gives the same screen (the procedure is in the comment at the top of the script).`,
  secStatus: "Status",
  stSteps: "Instructions",
  stVtime: "Virtual time",
  stRatio: "Real-time ratio",
  stSpeed: "Speed",
  stIdle: "Idle",
  secKeys: "Keys (PC keyboard)",
  kArrows: "Arrow keys / OK",
  kApp: "App1–App5",
  kAlnum: "Alphanumerics",
  kSym: "Symbols (which character they give depends on WM5's key layout)",
  kEdit: "Editing keys",
  kMod: "Modifier keys",
  keysNote: `Turn off your IME (keys during composition are not sent).
      For the soft keys (the left and right menus at the bottom of the screen), tap the screen. Symbols not in the table above are typed with WM5's on-screen keyboard.`,
  secLog: "Log",
  uart: "Kernel debug output (UART1)",
};

// コード用（日本語の文 → 英語）
const EN = {
  "このブラウザでは WebAssembly が使えません（iOS のロックダウンモードでは無効になります）。":
    "WebAssembly is not available in this browser (it is disabled in iOS Lockdown Mode).",
  "Worker の異常終了: {m}": "Worker crashed: {m}",
  "Worker が異常終了しました: {m}": "The worker crashed: {m}",
  "起動: {name}（命令 {steps}）": "Booted: {name} (instructions {steps})",
  "エミュレーションが止まりました。\n{m}": "Emulation stopped.\n{m}",
  "記録を止めました: {base}": "Recording stopped: {base}",
  "（カード未挿入）": " (no card inserted)",
  "・接続 {n}": ", connections {n}",
  "準備中…": "Preparing…",
  "起動しています…": "Booting…",
  "非表示": "hidden",
  "復帰": "return",
  "定期": "periodic",
  "読み込み": "load",
  "再開": "Resume",
  "一時停止": "Pause",
  "一時停止中": "Paused",
  "{n} 倍": "{n}x",
  "{n}M 命令/秒": "{n}M instr/s",
  "{n}% をスキップ": "{n}% skipped",
  "JIT 停止": "JIT stopped",
  "早送り": "Fast-forward",
  "記録停止": "Stop recording",
  "記録開始": "Start recording",
  "{n}G 命令": "{n}G instr",
  "＋ 新しいプロファイル…": "+ New profile…",
  "イメージ {name}": "Image {name}",
  "画面 {size}": "Screen {size}",
  "・": ", ",
  "まだ起動していません（下のイメージから起動します）": "Not booted yet (boot from an image below)",
  "新しいプロファイルの名前（例: WM6 VGA）": "Name of the new profile (e.g. WM6 VGA)",
  "プロファイルの名前": "Profile name",
  "プロファイル「{name}」を消しますか？（保存・ストレージカード・記録ごと消えます。イメージは残ります）":
    "Delete profile \"{name}\"? (Its saves, storage card and recordings are deleted too. Images are kept.)",
  "保存から再開": "Resume from save",
  "{name} から起動": "Boot from {name}",
  "この端末から消す": "Remove from this device",
  "{name} をこの端末から消しますか？": "Remove {name} from this device?",
  "イメージを選んでください。": "Please select an image.",
  "自動": "Auto",
  "手動": "Manual",
  "この保存から再開しますか？（今の状態は失われます。必要なら先に保存してください）":
    "Resume from this save? (The current state will be lost. Save it first if needed.)",
  "書き出す": "Export",
  "消す": "Delete",
  "この保存を消しますか？": "Delete this save?",
  "この端末の保存領域: {used}MB 使用 / 上限 {quota}GB": "Storage on this device: {used}MB used / {quota}GB limit",
  "中継サーバーの URL を入れてください": "Please enter the relay server URL",
  "挿しています（{name}{share}）。中身はエミュレータの中にあります。":
    "Inserted ({name}{share}). The contents are inside the emulator.",
  "カード": "card",
  "・フォルダ共有の方式": ", folder-sharing method",
  "抜いています: {name}（{size}・空き {free}）": "Ejected: {name} ({size}, {free} free)",
  "カードがありません。「作る」で空のカードを作るか、イメージを読み込んでください。":
    "No card. Create an empty card with \"Create\" or load an image.",
  "上のフォルダ": "Parent folder",
  "カードから消す": "Remove from the card",
  "{name} を消しますか？{dir}": "Delete {name}?{dir}",
  "（中身ごと）": " (including its contents)",
  "今のカードの中身は消えます。新しいカードを作りますか？（必要なら先にイメージを書き出してください）":
    "The contents of the current card will be erased. Create a new card? (Export the image first if needed.)",
  "フォルダの名前": "Folder name",
  "今のカードをこのイメージで置き換えますか？": "Replace the current card with this image?",
  "ページを離れる": "leaving the page",
  "Service Worker を登録できません: {m}": "Cannot register the Service Worker: {m}",
  // Worker
  "イメージを保存できません: {m}": "Cannot save the image: {m}",
  "保存 {base}: 命令 {steps}、{mb}MB（写し {cap}ms・全体 {all}ms）":
    "Saved {base}: instructions {steps}, {mb}MB (copy {cap}ms, total {all}ms)",
  "OPFS を使えません: {m}": "OPFS is not available: {m}",
  "自動保存に失敗しました: {m}": "Autosave failed: {m}",
  "大きさが合いません": "The size does not match",
  "カードのイメージを読めません: {m}": "Cannot read the card image: {m}",
  "カードを抜いてから編集してください": "Eject the card before editing",
  "カードがありません（先に作るか読み込んでください）": "There is no card (create or load one first)",
  "時計を合わせた（{reason}）: {time}": "Clock set ({reason}): {time}",
  "HTTPS の中継用の CA を作っています（初回だけ。十数秒かかることがあります）…":
    "Creating the CA for HTTPS relaying (first time only; it can take ten-odd seconds)…",
  "HTTPS の中継用の CA を作った（WM5 の IE で http://10.0.2.2/ から入れる）":
    "Created the CA for HTTPS relaying (install it from http://10.0.2.2/ in WM5's IE)",
  "中継サーバーの URL が不正です: {m}": "The relay server URL is invalid: {m}",
  "中継サーバーに接続中…": "Connecting to the relay server…",
  "中継サーバーとの接続が切れました（5 秒後につなぎ直します）": "Disconnected from the relay server (reconnecting in 5 seconds)",
  "中継サーバーにつなげません（URL・トークン・サーバーの起動を確認してください。5 秒後に再試行）":
    "Cannot reach the relay server (check the URL, token and that the server is running; retrying in 5 seconds)",
  "中継サーバーの版が古い（{ver}）: cerulean relay を新しくしてください":
    "The relay server is outdated ({ver}): update cerulean relay",
  "中継サーバーにつながっています": "Connected to the relay server",
  "中継サーバーが拒否しました: {m}": "The relay server refused: {m}",
  "ストレージカードを抜いてからオンにしてください（PC カードのソケットは 1 つです。オンにした後に挿すと、ネットワークと同時に使える方式で挿せます）":
    "Eject the storage card before turning this on (there is only one PC card socket. If you insert the card after turning it on, it is inserted in a way that works together with networking)",
  "イーサネットカードを挿した": "Inserted the Ethernet card",
  "イーサネットカードを抜いた": "Ejected the Ethernet card",
  "起動の早送りの後": "after the boot fast-forward",
  "既定": "Default",
  "新しいプロファイル": "New profile",
  "プロファイルを読めません: {m}": "Cannot read the profiles: {m}",
  "そのプロファイルはありません": "No such profile",
  "プロファイルの切り替え": "profile switch",
  "プロファイル「{name}」に切り替えた": "Switched to profile \"{name}\"",
  "使っているプロファイルは消せません（先に切り替えてください）": "The profile in use cannot be deleted (switch first)",
  "既定のプロファイルは消せません": "The default profile cannot be deleted",
  "保存がありません": "There is no save",
  "自動保存": "Autosave",
  "保存": "Save",
  "{n} を読めません（壊れている可能性）: {m}": "Cannot read {n} (it may be corrupted): {m}",
  "再開": "Resume",
  "読める保存がありません": "There is no readable save",
  "停止時": "On stop",
  "読み込んだスナップショット": "Loaded snapshot",
  "カードを抜いてから作り直してください": "Eject the card before recreating it",
  "新しいカード（{mb}MB）を作った": "Created a new card ({mb}MB)",
  "カードを抜いてから読み込んでください": "Eject the card before loading",
  "カードに {n} 個のファイルを入れた": "Added {n} file(s) to the card",
  "エミュレータが動いていません": "The emulator is not running",
  "カードをフォルダ共有として挿した（ネットワークと同時に使える方式）":
    "Inserted the card as a shared folder (a method that works together with networking)",
  "カードを挿した": "Inserted the card",
  "カードを抜いた（中身を保存した）": "Ejected the card (saved its contents)",
};

let lang = "en";
let pref = "auto";

export function detect() {
  let p = "auto";
  try {
    p = localStorage.getItem(PREF_KEY) ?? "auto";
  } catch {}
  pref = p === "ja" || p === "en" ? p : "auto";
  if (pref !== "auto") return pref;
  const langs = (typeof navigator !== "undefined" && (navigator.languages?.length ? navigator.languages : [navigator.language])) || [];
  return String(langs[0] ?? "").toLowerCase().startsWith("ja") ? "ja" : "en";
}
export const getPref = () => pref;
export const getLang = () => lang;
export function setLang(l) {
  lang = l === "ja" ? "ja" : "en";
}
// 設定の保存（auto・ja・en）。決まった言語を返す。
export function setPref(p) {
  try {
    if (p === "auto") localStorage.removeItem(PREF_KEY);
    else localStorage.setItem(PREF_KEY, p);
  } catch {}
  const l = detect();
  setLang(l);
  return l;
}

export function t(ja, params) {
  let s = lang === "en" ? (EN[ja] ?? ja) : ja;
  if (params) s = s.replace(/\{(\w+)\}/g, (m, k) => (k in params ? params[k] : m));
  return s;
}

// index.html の data-i18n・data-i18n-title・data-i18n-aria・data-i18n-placeholder を現在の言語に合わせる。
export function applyDom(root = document) {
  document.documentElement.lang = lang;
  for (const [attr, set] of [
    ["i18n", (el, v) => (el.innerHTML = v)],
    ["i18nTitle", (el, v) => (el.title = v)],
    ["i18nAria", (el, v) => el.setAttribute("aria-label", v)],
    ["i18nPlaceholder", (el, v) => el.setAttribute("placeholder", v)],
  ]) {
    const name = attr.replace(/[A-Z]/g, (c) => "-" + c.toLowerCase());
    for (const el of root.querySelectorAll(`[data-${name}]`)) {
      const orig = `ja${attr}`; // 元の日本語を最初に控える
      if (el.dataset[orig] === undefined) {
        el.dataset[orig] = attr === "i18n" ? el.innerHTML : attr === "i18nTitle" ? el.title : attr === "i18nAria" ? (el.getAttribute("aria-label") ?? "") : (el.getAttribute("placeholder") ?? "");
      }
      const v = lang === "en" ? EN_HTML[el.dataset[attr]] : el.dataset[orig];
      set(el, v ?? el.dataset[orig]);
    }
  }
}
