# onprem-rag-app

The **[Tauri](https://tauri.app/) v2** desktop + Android client for the on-prem health-records RAG
system. It's the UI for logging in, selecting a chat model, managing source databases, running
ingestion, and chatting with the data.

- **`src/`** — React 19 + Vite 7 + TypeScript frontend. Renders the UI and manages state. It
  **never** calls the server or holds the JWT directly.
- **`src-tauri/`** — the Rust **bridge**. Holds the server URL + JWT in managed state and makes the
  actual HTTP calls with `reqwest`, relaying the server's SSE streams to the frontend as Tauri
  events (`chat://token`, `ingest://progress`, `logs://line`, …).

See the repo root [`CLAUDE.md`](../CLAUDE.md) for the full architecture.

## Prerequisites

- **Node.js** 18+ and **pnpm** (`corepack enable pnpm`, or `npm i -g pnpm`). This workspace is
  pnpm-based — `pnpm-lock.yaml` is the tracked lockfile and `tauri.conf.json` shells out to `pnpm`.
- **Rust** (stable) + the Tauri v2 system dependencies for your OS — see the
  [Tauri prerequisites guide](https://tauri.app/start/prerequisites/). On Windows that's the
  **WebView2** runtime (preinstalled on Windows 11) and the **MSVC** build tools.
- **A running `onprem-rag-server`** — the app is a thin client over it. Start the server (and its
  DocumentDB) first; see [`../onprem-rag-server/README.md`](../onprem-rag-server/README.md).

## 1. Configure

The bridge defaults to `http://localhost:8000`. To point elsewhere, set the server URL the
frontend pushes down on startup via this project's own `.env` (Vite reads it from **this**
directory, not the repo root):

```bash
# onprem-rag-app/.env
VITE_DEFAULT_SERVER_URL=http://localhost:8000
```

You can also change the server URL at runtime from the app's settings.

## 2. Install & run (development)

```bash
cd onprem-rag-app
pnpm install
pnpm tauri dev
```

That launches the native window with hot-reload for the React frontend and rebuilds the Rust
bridge on change. Log in with the seeded admin (`admin` / `password` by default).

> **Frontend-only in a browser:** `pnpm dev` starts just the Vite dev server (no native
> shell). Useful for pure UI work, but any `@tauri-apps/api` `invoke` call fails outside the Tauri
> window — use `pnpm tauri dev` whenever you need the bridge (login, server calls, SSE).

Once logged in:

- **Settings** — inspect execution providers and download/select a chat model. The
  **Model & hardware activity** log window shows Foundry's download/load progress live.
- **Sources** — add a source database (Test → Save; the password is encrypted server-side), then
  **Ingest**. The **Ingestion activity** log window shows row fetch, the embedding-model
  download/load, index creation, and upserts.
- **Chat** — ask grounded questions; answers stream token-by-token with citations.

> The log windows stream the server's `tracing` output over SSE, so long external operations
> (e.g. the first BGE-M3 embedding-model download, ~2.3 GB) show progress instead of appearing to
> hang.

## 3. Build a release bundle

```bash
pnpm tauri build                 # full native installer/executable (frontend + bridge)
```

Produces a platform installer/executable under `src-tauri/target/release/`. Frontend-only build
steps (rarely needed on their own — `tauri build` runs them for you):

```bash
pnpm build                       # tsc && vite build → static frontend in dist/
pnpm preview                     # serve the built frontend to sanity-check the bundle
```

`pnpm tauri` is the raw Tauri CLI passthrough — handy for diagnostics and assets:

```bash
pnpm tauri info                  # environment + version report (attach this to bug reports)
pnpm tauri icon path/to.png      # regenerate app icons from a source image
```

## Android

The app also builds for Android. `src-tauri/gen/android` is already initialised and its **source is
committed** — the `AndroidManifest.xml`, `network_security_config.xml`, package id, and icons carry
hand edits the build depends on. The regenerable parts (`build/`, `.gradle/`, `local.properties`,
signing keys) are gitignored. **Do not** re-run `tauri android init` — it would overwrite those edits.

### Android prerequisites

The **Android SDK + NDK** must be installed, with these environment variables set (adjust the paths
to your install):

```bash
# Linux / macOS (bash/zsh)
export ANDROID_HOME="$HOME/Android/Sdk"
export NDK_HOME="$ANDROID_HOME/ndk/<version>"        # e.g. .../ndk/27.1.12297006

# Windows (PowerShell)
$env:ANDROID_HOME = "$env:LOCALAPPDATA\Android\Sdk"
$env:NDK_HOME     = "$env:ANDROID_HOME\ndk\<version>"
```

The quickest way to get the SDK + NDK is **Android Studio** (SDK Manager → install the SDK Platform,
Platform-Tools, and NDK). `adb` and `emulator` live under `$ANDROID_HOME/platform-tools` and
`$ANDROID_HOME/emulator` — add both to your `PATH`.

> **JDK: use Java 17–24, not Android Studio's bundled JBR.** The Gradle wrapper pinned by the
> Android scaffold (8.14.3) **cannot run on Java 25** — it dies before the build starts with
> `BUG! ... Unsupported class file major version 69`. Android Studio's bundled JBR *is* Java 25, so
> if `JAVA_HOME` points at it (`…\Android Studio\jbr`), the Android build fails. Install a JDK 21
> (LTS) and point `JAVA_HOME` at it:
>
> ```powershell
> winget install EclipseAdoptium.Temurin.21.JDK
> # persist for new terminals (user scope); Android Studio's IDE is unaffected — it has its own Gradle JDK setting
> [Environment]::SetEnvironmentVariable("JAVA_HOME", "C:\Program Files\Eclipse Adoptium\jdk-21.0.12.101-hotspot", "User")
> ```
>
> If Gradle still reports Java 25 after changing `JAVA_HOME`, a stale Gradle **daemon** on the old
> JVM is being reused — kill it (`Get-CimInstance Win32_Process -Filter "Name='java.exe'" | Stop-Process`)
> and re-run. As a belt-and-suspenders alternative that ignores the shell environment entirely, pin
> the JDK in `src-tauri/gen/android/gradle.properties`:
> `org.gradle.java.home=C:/Program Files/Eclipse Adoptium/jdk-21.0.12.101-hotspot` (note: that file
> is regenerated by `tauri android init`, so the persisted `JAVA_HOME` above is the durable fix).

### 1. Have a device or emulator running

```bash
adb devices                       # list attached devices — the target must show as "device"

# …or start an emulator:
emulator -list-avds               # AVDs you've created in Android Studio
emulator -avd <avd-name>          # boot one
```

### 2. Run (development, hot-reload)

```bash
pnpm tauri android dev            # build, install, and launch on the connected device/emulator
```

This bundles the frontend, compiles the Rust bridge for Android, installs the debug APK, and
hot-reloads the React frontend on change — the mobile equivalent of `pnpm tauri dev`. The debug APK
is auto-signed with the Android debug keystore, so it installs on any device without extra setup.

### 3. Build an APK (release)

```bash
pnpm tauri android build          # default: builds both APK + AAB, all ABIs (universal)
pnpm tauri android build --apk    # APK only (skip the Play-Store AAB) — the common case for
                                  # sideloading / on-prem distribution
```

Useful flags (`pnpm tauri android build --help` for the full list):

| Flag                  | Effect                                                                                                                                        |
| --------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| `--apk` / `--aab` | Build only that format (default builds both).                                                                                                 |
| `--debug`           | Build the**debug** variant (debug-signed, installs immediately — handy for a quick shareable build before you set up release signing). |
| `--split-per-abi`   | Emit one smaller APK per ABI (`arm64-v8a`, `armeabi-v7a`, `x86`, `x86_64`) instead of one universal APK.                              |
| `--target <triple>` | Build a single architecture, e.g.`aarch64` for most phones.                                                                                 |

Outputs land under `src-tauri/gen/android/app/build/outputs/`:

- **APK** (universal) — `apk/universal/release/app-universal-release.apk`
- **AAB** (Play Store upload) — `bundle/universalRelease/app-universal-release.aab`
- with `--split-per-abi`, per-ABI APKs — `apk/<abi>/release/app-<abi>-release.apk`

Install a built APK on a connected device manually with:

```bash
adb install -r src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk
```

> **A `release` APK must be signed to install.** Out of the box the release variant has **no signing
> config**, so `pnpm tauri android build` emits `app-universal-release-unsigned.apk`, which Android
> refuses to install (`INSTALL_PARSE_FAILED_NO_CERTIFICATES`). Either build `--debug` for a quick
> shareable build, or configure release signing once (next section).

### 4. Sign the release APK

Do this once to produce installable/distributable release builds. The keystore and its passwords are
**secrets** — the paths below (`*.jks`, `keystore.properties`) are gitignored; never commit them.

**1. Generate a keystore** (keep the `.jks` somewhere safe and backed up — losing it means you can
never ship an update to the same app identity):

```bash
keytool -genkey -v -keystore ~/onprem-rag-release.jks \
  -keyalg RSA -keysize 2048 -validity 10000 -alias onprem-rag
```

**2. Point the build at it** — create `src-tauri/gen/android/keystore.properties` (gitignored):

```properties
storeFile=/absolute/path/to/onprem-rag-release.jks
storePassword=<the store password you set>
keyAlias=onprem-rag
keyPassword=<the key password you set>
```

**3. Wire it into** `src-tauri/gen/android/app/build.gradle.kts` (this file **is** tracked, so the
wiring persists; only the properties/keystore stay out of git). Load the props near the top, add a
`signingConfigs` block, and reference it from the `release` build type:

```kotlin
// near the existing `tauriProperties` block
val keystoreProperties = Properties().apply {
    val f = rootProject.file("keystore.properties")
    if (f.exists()) f.inputStream().use { load(it) }
}

android {
    // …
    signingConfigs {
        create("release") {
            keyAlias = keystoreProperties["keyAlias"] as String
            keyPassword = keystoreProperties["keyPassword"] as String
            storeFile = file(keystoreProperties["storeFile"] as String)
            storePassword = keystoreProperties["storePassword"] as String
        }
    }
    buildTypes {
        getByName("release") {
            signingConfig = signingConfigs.getByName("release")
            // …existing isMinifyEnabled / proguardFiles…
        }
    }
}
```

Re-run `pnpm tauri android build --apk` — the output is now `app-universal-release.apk` (signed),
ready for `adb install -r`.

### Connecting to the server from the phone

**Point the app at the server's LAN IP, not `localhost`.** On a phone, `localhost` is the phone
itself — it won't find your dev machine's server. Set the server URL from the in-app **"Connect to
server"** screen, or bake in a default via this project's `.env`:

```bash
# onprem-rag-app/.env
VITE_DEFAULT_SERVER_URL=http://<host-lan-ip>:8000
```

The server binds `0.0.0.0:8000`, so it's reachable on the LAN. Cleartext HTTP is enabled in the
Android manifest (`android:usesCleartextTraffic="true"`) precisely because on-prem servers are
reached over plain HTTP on a LAN IP, and the server URL is chosen at runtime (any LAN IP) — Android's
network-security config keys on concrete domains, so it canno.t scope cleartext to an IP range.

> **On the emulator (AVD), use `http://10.0.2.2:8000`.** Inside the emulator `localhost` is the
> emulator VM itself; `10.0.2.2` is its built-in alias for the **host machine's** loopback. (The
> host's LAN IP works too, but `10.0.2.2` doesn't depend on Wi-Fi/subnet.) If it still can't reach
> the host, the **Windows Firewall** is likely blocking inbound on 8000 — add an inbound rule for
> that port. This alias is emulator-only; a physical phone uses the host LAN IP as described above.

### Securing the phone connection (production)

Plain HTTP is only acceptable on a **trusted, segmented LAN**. For anything beyond that, terminate TLS
at a reverse proxy (nginx/Caddy) in front of the server, give it a real hostname + certificate, then lock
the app down in one file — `src-tauri/gen/android/app/src/main/res/xml/network_security_config.xml`:

1. set `cleartextTrafficPermitted="false"` on the `<base-config>`,
2. uncomment the `<domain-config>` and set `<domain>` to the proxy's hostname,
3. optionally pin the proxy certificate in the `<pin-set>` so a rogue CA cannot MITM the link.

That confines the app to HTTPS-to-a-known-host and closes LAN sniffing of the JWT and PHI. The config
file is already wired into the manifest via `android:networkSecurityConfig`; the steps above are the whole
change. (Server-side TLS termination is covered in [`../onprem-rag-server/README.md`](../onprem-rag-server/README.md).)

> **No LAN? Use a USB tunnel.** `adb reverse tcp:8000 tcp:8000` forwards the phone's `localhost:8000`
> to the dev machine over USB — then `VITE_DEFAULT_SERVER_URL=http://localhost:8000` works on the
> device without exposing the server on the network.
>
> **Known follow-up (not yet wired):** the Android hardware **back button** is not yet bound to the
> in-app nav stack's `back()`. Tauri v2 has no stable JS API for the Android back gesture yet; the
> nav `back()` exists in the UI store, but binding it to the OS back button is deferred to
> on-device work.

## Quick checks

```bash
npx tsc --noEmit                        # frontend type-check
cd src-tauri && cargo check             # bridge type/borrow check
cd src-tauri && cargo fmt               # format the bridge (cargo fmt --check to verify only)
cd src-tauri && cargo clippy            # lint the bridge
```

## Recommended IDE setup

[VS Code](https://code.visualstudio.com/) +
[Tauri](https://marketplace.visualstudio.com/items?itemName=tauri-apps.tauri-vscode) +
[rust-analyzer](https://marketplace.visualstudio.com/items?itemName=rust-lang.rust-analyzer).
