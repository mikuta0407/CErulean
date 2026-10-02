# Detailed usage

[日本語](usage.md) | **English**

[Back to the README](../README.en.md).

## Getting started

Download and extract the prebuilt binary for your environment from [Releases](https://github.com/mikuta0407/CErulean/releases).
Run it in the folder that contains `cerulean`.

```sh
./cerulean serve
```

On Windows, use `cerulean.exe` and run `./cerulean.exe serve` in PowerShell.
Open <http://127.0.0.1:8000/app/> in your browser and select the `.bin` you prepared following the [extraction guide](images.en.md).
The first boot takes tens of seconds or more.

Keep the terminal running the server open. Press Ctrl+C to stop it.
If you lack execute permission, run `chmod +x cerulean`.

### Building from source

To build from source, run the following commands at the root of this repository (assuming Bash on Linux / macOS,
or Bash on WSL for Windows).

Requirements:

- rustup (Rust **1.98.1** per `rust-toolchain.toml`; the wasm target is also installed automatically)
- A C compiler and linker (GCC or Clang on Linux / WSL, Xcode Command Line Tools on macOS)
- Node.js (24 LTS was used for development)
- wasm-bindgen-cli **0.2.129** (the same version as the wasm-bindgen in this repository)
- A [supported OS image](images.en.md)

```sh
# First time only. After setting up rustup and Node.js, install the CLI with the pinned Rust
cargo install wasm-bindgen-cli --version 0.2.129 --locked

# Generate the web assets and embed them in the standalone binary
bash tools/build.sh

# Keep this terminal open. Press Ctrl+C to stop
target/release/cerulean serve
```

1. Open **<http://127.0.0.1:8000/app/>** in a PC browser.
2. On the start screen, select the extracted **`.bin` file** (not the downloaded `.msi`).
   The screen size is detected from the file name with the default "Auto". Change it only if it is wrong.
3. After starting, wait for the Today screen. The first time takes tens of seconds or more
   (depending on the device and browser). Progress and the Windows Mobile boot screen are shown meanwhile.
4. Operate by clicking or tapping the screen. Arrow keys, Enter and the buttons below the screen also work.
   Turn off your IME when typing alphanumerics on a PC.

After startup the state is saved automatically, and you can "Resume" next time. Turn sound on with
"Play sound" in the menu (off by default). See the browser app usage below for details.

## Using the internet

A relay server is required to connect to the internet from inside Windows Mobile.
The following command starts the browser app and the relay server together.

```sh
./cerulean serve --with-relay
```

1. Open the URL with a token that is shown at startup in your browser.
2. Start Windows Mobile, and turn on "Network (via relay server)" in the CErulean menu.
3. In Windows Mobile, go to "Settings → Connections → Network Card" and set the destination to "The Internet".
4. Open a website in Internet Explorer on Windows Mobile.
   To use HTTPS sites, first open `http://10.0.2.2/` and install the CErulean Local CA certificate.

To use a different relay server, specify its URL and token in the menu.
From an HTTPS page, use `wss://` (`ws://127.0.0.1` for a local relay).

## Using on a smartphone

Open the served CErulean in your browser.
Save the extracted `.bin` on the smartphone and select it on that device.

On iOS, performance may be slow unless the page is opened over HTTPS.

## Features

- WM5: Pocket PC English and Japanese editions, Japanese VGA edition
- WM6: the Classic configuration included in the Japanese Professional Images, and the Japanese VGA configuration with phone UI
- Boot to the Today screen, use the standard apps, install supported CAB / EXE files
- Tap and drag on the screen; arrow keys, Enter, alphanumerics and App button input
- Automatic and manual save, resume, snapshot import / export
- Display language (Japanese / English): detected automatically from the browser and switchable from the menu
- Per-image and per-screen-size profile switching, adding to the home screen (PWA), offline startup
- Guest audio playback (at normal speed), clock synchronization with the device
- Creating and inserting / ejecting storage cards, and exchanging files with the host
- CLI script execution, recording and replay of operations, PNG screen output, WAV audio output, folder sharing
- IPv4 TCP communication via a relay, DHCP / DNS, HTTP / HTTPS in IE Mobile
  (HTTPS requires installing a dedicated CA in the guest; see the networking description above)
- In-browser JIT-to-wasm (enabled by default; can be switched to the interpreter in the menu)
- Web hosting from a standalone binary, or from static files only

## Limitations

- Booting real-device ROMs, Windows Phone and desktop Windows. Supported configurations are the S3C2410-based Device Emulator ones.
- Calls, SMS and cellular connections. Even phone-enabled images do not implement a communication modem.
- Guest internet access from the browser without a relay. With static hosting only, use it with networking turned off.
- IPv6, UDP other than DNS, outbound ICMP, and inbound connections to the guest (server use).
- Hardware soft key (VK_F1/F2) and power button input. Tap the on-screen buttons instead of the soft keys.
- Loading `.msi` directly. Extract the `.bin` beforehand.
- Running in multiple tabs at once, and continuing a recording after a forced exit during recording. Resuming from a saved state is possible.
- No guarantee that every app or peripheral works. Sound does not play during fast-forward, 2x or maximum speed.

## TODO

- [ ] Measure JIT memory use per browser and tune the limit on generated code size
- [ ] Decide the minimum supported browser versions and CSP settings for public hosting
- [ ] Implement input paths for the hardware soft keys and power button
- [ ] Fill in unsupported ARM / MMU and peripheral behavior (alignment checks, additional NOR flash commands, etc.)
- [ ] Native interactive UI (iOS / Android / PC). The current native build is a development CLI
- [ ] Support for PXA27x-based real-device configurations (future extension)

There is no schedule for addressing the TODOs.

## Saving and troubleshooting

Images and saved states are stored in the browser's storage (OPFS). Merely selecting an image
does not send it anywhere. Clearing site data also deletes your saves, so export any state you want to keep
from the menu. **Snapshots also contain the contents of the OS image, so do not share them publicly.**

- If the wasm-bindgen version does not match when building, reinstall the pinned version above.
- If saving is unavailable, open it at `http://127.0.0.1` / `http://localhost` on a PC.
  Using it from another device such as a smartphone requires HTTPS hosting; see the hosting examples below.
- If the screen size looks wrong, specify the resolution matching the image on the start screen.
- A second tab waits. Close the tab that was opened first.

## Hosting

### Serve from the standalone binary

You can copy `target/release/cerulean`, built with `tools/build.sh`, anywhere and use it.
There is no need to place HTML, JS or wasm next to it. The web assets used at build time are embedded,
so it is unaffected by the directory you run it from.

```sh
./cerulean serve                 # http://127.0.0.1:8000/app/
./cerulean serve --with-relay    # also serves the guest network relay at /relay on the same port
```

You can specify `--port N` (127.0.0.1) and `--listen 0.0.0.0:8000` (for other devices).
To try changes to the web assets, switch to serving external files with `--root web/www`.
With `--with-relay` the relay also runs, and opening the `…/app/#relay-token=…` URL shown at startup
fills in the token. The relay URL defaults to "/relay on this site", so you only need to turn on
networking in the menu. By default the relay does not connect to private addresses (LAN, localhost, etc.);
allow them with `--allow-private`. `--token` fixes the token.

### Serve from static files only

**If the guest network relay is not needed, an ordinary static web server is enough.**
Emulation, input, saving and audio are handled in the browser, so there is no need to keep a CErulean
server running all the time.

```sh
# Write out the same web assets as the embedded ones into a new directory
./cerulean web-export site
```

Place the contents of the generated `site/` as-is in the web server's public directory.
**Both `app/` and `pkg/` (including `snippets/` inside it) are required.**
Open `/app/` on the site. Placing it in a subdirectory such as `/demo/app/` also works.
Do not put OS images in the public directory; users select them in the browser.

To serve web assets you built yourself, after `bash tools/web-build.sh` you can also copy
`web/www/index.html`, `web/www/app/` and `web/www/pkg/` in the same layout.

Serve over **HTTPS**, and set the `.wasm` Content-Type to **`application/wasm`** and `.js` to
**`text/javascript`**. `http://localhost` can also be used for local checks.
Opening the HTML directly via `file://` is not supported. Keep networking at its default off setting.
To use the internet from inside Windows Mobile, a relay server is required. When needed, prepare `./cerulean relay` separately and set its URL and token.

### Serving HTTPS and the relay together

An example of publishing it (TLS is added by a reverse proxy). Caddy passes WebSockets through as-is, so you only need to
send everything to a single port:

```
cerulean.example.net {
    reverse_proxy 192.168.1.11:8000     # cerulean serve --listen 0.0.0.0:8000 --with-relay --token <long token>
}
```

On an https page, OPFS and SubtleCrypto are available, and the relay is reached automatically via `wss://…/relay`. When publishing,
use a long token and, if needed, restrict `/relay` by IP (Caddy's `remote_ip`). The benchmark page (bench) is served with
`tools/serve-bench.py 8000` (the app can also be opened at `http://localhost:8000/web/www/app/`).

## Browser app in detail

When you select an image, it boots to Today at maximum speed (tens of seconds or more depending on the device), saves
automatically on arrival, and then runs at normal speed. Until the Windows Mobile splash appears (while the guest screen is black),
a boot screen shows the progress and the kernel debug output. Operate it with taps on the screen (mouse / touch), the buttons
below the screen (to the right in landscape) (arrow keys, OK, App1–5), and PC keys (arrows, Enter, alphanumerics, F1–F5 = App1–5, etc.;
turn off the IME).

- Saving: images and snapshots are kept in the device's OPFS (not sent anywhere). Autosave keeps
  3 generations (on arriving at Today, when leaving the screen, every 30 seconds if there was input, otherwise every 5 minutes).
  "Resume" appears next time you open it. From the menu you can save manually, export (.snap.gz) and import.
  **Exported snapshots contain the contents of the image**, so do not share them in public.
- Recording: "Start recording" in the menu takes a starting snapshot, and "Stop recording" produces an input
  script (absolute instruction counts). Replaying the two exported files with the CLI reproduces the same state
  (the comment at the top of the script has the procedure and the SHA-256 of the final CPU, RAM and screen):
  `gunzip X.snap.gz && cerulean run --snap-load X.snap --script X.txt --result r.jsonl PPC_USA.bin`
- Sound: "Play sound" in the menu plays the guest's sounds (startup sound, tap sounds, notifications, etc.; IIS + DMA)
  (off by default; played only at normal speed, not during fast-forward, 2x or maximum speed).
- Clock: after resuming from a save, returning to the screen, or after the boot fast-forward, the guest clock is synced to the
  device time (can be turned off in the menu; it is also recorded as an `rtc` command, so replay gives the same result).
- Profiles: use environments with different OSes and screen sizes (WM5 USA, WM6 JPN, VGA, etc.) in a single PWA.
  Create, switch, rename and delete them on the first screen (via "To first screen" in the menu). Saves,
  storage cards and recordings are per profile; images are shared. Switching autosaves and stops the current state,
  then resumes from the newest save of the destination (only one runs at a time).
- Only one tab can run (a second tab waits). It can be added to the home screen and opened offline.
