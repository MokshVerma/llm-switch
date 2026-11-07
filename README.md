# LLM Switch

A small desktop app (and command-line tool) for running a local LLM through
[llama.cpp](https://github.com/ggml-org/llama.cpp)'s `llama-server`, written in Rust.

- **One-click start / stop / switch** for any `.gguf` model in a folder.
- **Per-model settings** (context size, GPU layers, MoE expert offload, KV-cache type, ...) kept in one config file.
- **OpenCode sync**: writes the model's context limit and compaction buffer into `opencode.json` on every start.
- **Auto-tune**: restarts the server with trial settings, measures real VRAM use and speed, and recommends the
  most GPU-heavy configuration that still leaves headroom.

It ships as two programs built from the same code:

| Program | What it is |
|---|---|
| `llm-switch` | the window (GUI) |
| `llm-switch-cli` | the same actions from a terminal, for scripts and shortcuts |

> **Status:** developed and tested on **Windows 11 + an NVIDIA GPU**. The macOS build is verified in CI: GitHub's
> macOS runner builds a universal (Apple Silicon + Intel) `LLM Switch.app` that passes `codesign --verify`. But
> **nobody has launched it on a Mac yet**, and the Linux code has only been type-checked
> (`cargo check` for `x86_64-unknown-linux-gnu`). See [Platform support](#11-platform-support).

---

## Contents

1. [What you need to run it](#1-what-you-need-to-run-it)
2. [Build on Windows](#2-build-on-windows)
3. [Build on macOS](#3-build-on-macos)
4. [Build on Linux](#4-build-on-linux-untested)
5. [First run and configuration](#5-first-run-and-configuration)
6. [Using the app](#6-using-the-app)
7. [Command-line reference](#7-command-line-reference)
8. [Auto-tune](#8-auto-tune)
9. [OpenCode integration](#9-opencode-integration)
10. [Project layout](#10-project-layout)
11. [Platform support](#11-platform-support)
12. [Troubleshooting](#12-troubleshooting)
13. [Development notes](#13-development-notes)
14. [License](#14-license)

---

## 1. What you need to run it

LLM Switch does not include a model or an inference engine. You need two things, and **the app can fetch both for
you**: on a first run it opens on the **Setup** tab, detects your hardware, installs the right llama.cpp build, and
downloads a model from a link you paste. See [First run: the Setup tab](#first-run-the-setup-tab). To do it by hand:

1. **`llama-server`** from llama.cpp.
   - **Windows:** download a release zip from <https://github.com/ggml-org/llama.cpp/releases>. For an NVIDIA GPU
     take the `win-cuda-*-x64` zip **and** the matching `cudart-*` zip, and unzip both into the same folder
     (for example `D:\llama.cpp`). The app was tested with build b12128 (CUDA 13.4).
   - **macOS:** `brew install llama.cpp` (installs `/opt/homebrew/bin/llama-server` on Apple Silicon), or build it
     from source with Metal enabled.
2. **A `.gguf` model** in a folder of your choice (for example `D:\models` or `~/models`).
3. *(Optional)* **[OpenCode](https://opencode.ai)** if you want the automatic `opencode.json` sync.
4. *(Optional, auto-tune only)* an **NVIDIA GPU with `nvidia-smi`** on the PATH. Auto-tune is not available on
   macOS yet (see [Auto-tune](#8-auto-tune)).

---

## 2. Build on Windows

### Prerequisites

| Tool | Why | How to get it |
|---|---|---|
| **Rust** 1.85 or newer (the project uses the 2024 edition; developed with 1.99) | the compiler | <https://rustup.rs>, choose the default `x86_64-pc-windows-msvc` toolchain |
| **Visual Studio Build Tools** with the **"Desktop development with C++"** workload | the MSVC linker, and `rc.exe` (the Windows SDK resource compiler) used to embed the icon | <https://visualstudio.microsoft.com/visual-cpp-build-tools/> |

Install the Build Tools **before** Rust if you can; `rustup-init` will offer to install them for you. Open a new
terminal afterwards so `cargo` is on the PATH. Check:

```powershell
rustc --version
cargo --version
```

### Build

```powershell
git clone <this repository> llm-switch      # or just unzip the source
cd llm-switch
cargo build --release
```

The first build downloads and compiles about 400 crates and takes a few minutes; later builds take seconds.

Outputs:

```
target\release\llm-switch.exe        the window
target\release\llm-switch-cli.exe    the command-line tool
```

Both carry the app icon (`assets/icon.ico`), embedded by `build.rs` through `icon.rc`.

### Install

There is no installer. Copy the two exes anywhere you like, for example:

```powershell
New-Item -ItemType Directory -Force D:\llm-switch | Out-Null
Copy-Item target\release\llm-switch.exe, target\release\llm-switch-cli.exe D:\llm-switch\ -Force
```

Windows will not let you overwrite an exe that is running, so close the window before copying a new build over it.

To get a Desktop shortcut:

```powershell
$ws = New-Object -ComObject WScript.Shell
$s = $ws.CreateShortcut("$([Environment]::GetFolderPath('Desktop'))\LLM Switch.lnk")
$s.TargetPath = "D:\llm-switch\llm-switch.exe"
$s.WorkingDirectory = "D:\llm-switch"
$s.IconLocation = "D:\llm-switch\llm-switch.exe,0"
$s.Save()
```

### Windows build problems

| Symptom | Fix |
|---|---|
| `linker 'link.exe' not found` | Install the Build Tools workload above, then open a **new** terminal. |
| `RC.EXE not found` or a failure in `embed-resource` | The Windows SDK is missing. In the Visual Studio Installer, tick a "Windows 10/11 SDK" under the C++ workload. |
| The exe flashes a console window | You built in debug mode. The GUI only hides its console in **release** builds (`cargo build --release`). |
| `cargo` not recognised | Open a new terminal, or run `$env:Path += ";$env:USERPROFILE\.cargo\bin"`. |

---

## 3. Build on macOS

> The CI workflow has built this app successfully on a macOS runner: both binaries are universal (`arm64` and
> `x86_64`), the bundle contains the icon and `Info.plist`, and its ad-hoc signature verifies. What has **not**
> been done is launching it on a real Mac, so window behaviour, the Dock icon and the `lsof`/`open` calls are
> untested. If something breaks, please report it.

### Prerequisites

| Tool | How to get it |
|---|---|
| **Xcode Command Line Tools** (compiler, linker, `codesign`, `lipo`, `ditto`) | `xcode-select --install` |
| **Rust** 1.85 or newer | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh`, then open a new terminal |
| *(universal builds only)* both Mac targets | `rustup target add aarch64-apple-darwin x86_64-apple-darwin` |

macOS 11 (Big Sur) or later is required by the bundle (`LSMinimumSystemVersion`).

### Quick build (a plain binary)

```bash
git clone <this repository> llm-switch      # or unzip the source
cd llm-switch
cargo build --release
./target/release/llm-switch                  # opens the window
./target/release/llm-switch-cli list
```

### Build the `.app` bundle

`packaging/macos/bundle.sh` builds the binaries, assembles `LLM Switch.app`, signs it, and zips it.

```bash
chmod +x packaging/macos/bundle.sh           # only needed if the executable bit was lost
./packaging/macos/bundle.sh                  # this Mac's architecture
./packaging/macos/bundle.sh --universal      # Apple Silicon + Intel in one app
```

Output:

```
dist/LLM Switch.app
dist/LLM-Switch-0.1.0-macos.zip
```

Install by dragging `LLM Switch.app` into `/Applications`.

What goes into the bundle:

```
LLM Switch.app/
  Contents/
    Info.plist              generated from packaging/macos/Info.plist.in (version comes from Cargo.toml)
    PkgInfo
    MacOS/
      llm-switch            the window (this is what launches)
      llm-switch-cli        the command-line tool
    Resources/
      icon.icns             assets/icon.icns
```

Script options:

| Option | Meaning |
|---|---|
| `--universal` | Build `aarch64-apple-darwin` and `x86_64-apple-darwin` and merge them with `lipo`. |
| `--target TRIPLE` | Build one specific target instead of this Mac's. |
| `--sign IDENTITY` | `codesign` identity. Default `-` (ad-hoc). |
| `--out DIR` | Output folder (default `dist`). |
| `--skip-build --bin-dir DIR` | Skip cargo and bundle ready-made `llm-switch` and `llm-switch-cli` binaries from `DIR`. |

### Signing, Gatekeeper and notarization

- **For your own Mac:** the default ad-hoc signature (`--sign -`) is enough. If you copied the app from another
  machine (or downloaded the zip), macOS quarantines it. Either right-click the app, choose **Open**, then **Open**
  again, or clear the flag:

  ```bash
  xattr -dr com.apple.quarantine "/Applications/LLM Switch.app"
  ```

- **To give it to other people without a warning** you need an Apple Developer ID certificate and notarization:

  ```bash
  ./packaging/macos/bundle.sh --universal --sign "Developer ID Application: Your Name (TEAMID)"

  # Notarize the zip, wait for the result, then staple the ticket to the app and re-zip.
  xcrun notarytool submit dist/LLM-Switch-0.1.0-macos.zip \
      --apple-id you@example.com --team-id TEAMID --password "<app-specific password>" --wait
  xcrun stapler staple "dist/LLM Switch.app"
  ditto -c -k --keepParent "dist/LLM Switch.app" dist/LLM-Switch-0.1.0-macos.zip
  ```

### Build the `.app` in CI instead (no Mac needed)

`.github/workflows/release.yml` builds the macOS app (universal) on a GitHub-hosted macOS runner, and the Windows
exes on a Windows runner, then publishes both. See [Automated builds and releases](#automated-builds-and-releases).
The macOS app uses the ad-hoc signature, so the Gatekeeper note above still applies.

### Using the command-line tool from a terminal

Link the bundled CLI somewhere on your PATH:

```bash
ln -s "/Applications/LLM Switch.app/Contents/MacOS/llm-switch-cli" /usr/local/bin/llm-switch-cli
```

### Changing the macOS icon

`assets/icon.icns` is checked in. To regenerate it from the drawing code, run `packaging/make-icns.ps1` on
**Windows** (it uses .NET drawing and writes the ICNS container by hand, so no Mac tools are needed). To use your
own artwork, replace `assets/icon.icns` with any valid `.icns` file and rebuild the bundle.

---

## 4. Build on Linux (untested)

The code has a Linux branch (`xdg-open`, `lsof`, `lspci`, `nvidia-smi`, `~/.config/llm-switch`). Its OS-specific
modules type-check for `x86_64-unknown-linux-gnu`, but it has never been run.

```bash
# Debian / Ubuntu
sudo apt install build-essential pkg-config libxkbcommon-dev libwayland-dev libx11-dev libxcursor-dev \
                 libxi-dev libxrandr-dev libgl1-mesa-dev
cargo build --release
./target/release/llm-switch
```

---

## 5. First run and configuration

On first launch the app writes a default config file and uses it from then on.

| OS | Config file |
|---|---|
| Windows | `%APPDATA%\llm-switch\config.toml` (`C:\Users\<you>\AppData\Roaming\llm-switch\config.toml`) |
| macOS | `~/Library/Application Support/llm-switch/config.toml` |
| Linux | `~/.config/llm-switch/config.toml` (or `$XDG_CONFIG_HOME/llm-switch/config.toml`) |

Set the environment variable **`LLM_SWITCH_CONFIG`** to use a different file. `llm-switch-cli config` prints the
path in use, and the window's **Open config** button opens it in your editor.

### First run: the Setup tab

A first run (or a missing llama-server or model) opens the window on the **Setup** tab. Everything in it is also
available from the command line (see [Command-line reference](#7-command-line-reference)).

**1. This machine.** The app looks at your OS, CPU architecture, RAM, and GPUs: for NVIDIA it reads the driver
version and compute capability from `nvidia-smi`; AMD, Intel and Apple GPUs come from the OS.

**2. llama.cpp.** If `llama-server` is already on your PATH, or where a package manager puts it, the app uses that.
Otherwise it looks through the newest [llama.cpp releases](https://github.com/ggml-org/llama.cpp/releases) for the
build that fits this machine:

| Machine | Build |
|---|---|
| Windows, NVIDIA, driver 580+ and a Turing-or-newer card (RTX 20 series and later) | CUDA 13.x, plus its CUDA runtime archive |
| Windows, NVIDIA, driver 527 to 579, **or** an older card (GTX 10 series and earlier) | CUDA 12.x, plus its runtime archive |
| Windows, NVIDIA driver older than 527 | Vulkan, with a note recommending a driver update |
| Windows, AMD or Intel GPU | Vulkan |
| Windows, no GPU | CPU |
| Windows on ARM | CPU (arm64) |
| macOS, Apple Silicon / Intel | Metal (arm64 / x64) |
| Linux, with a GPU / without | Vulkan / CPU. For CUDA or ROCm on Linux, build llama.cpp yourself and set `llama_server`. |

The download is checked against GitHub's published SHA-256 when there is one, unpacked (entries that try to escape
the install folder are skipped), and then **started once with `--version`** to prove it works. If Windows reports a
missing DLL, the message says to install the Microsoft Visual C++ Redistributable. The install goes to
`%LOCALAPPDATA%\llm-switch\llama.cpp` (Windows), `~/Library/Application Support/llm-switch/llama.cpp` (macOS) or
`~/.local/share/llm-switch/llama.cpp` (Linux) unless you pick another folder, and the config is pointed at it.
It does not need administrator rights.

**3. A model.** Paste any of these into the link box and press **Look up**:

| You paste | What happens |
|---|---|
| A Hugging Face repository (`https://huggingface.co/owner/model-GGUF` or `owner/model-GGUF`) | Lists its `.gguf` files with sizes so you can pick a quantization. Split models are grouped into one entry. |
| A Hugging Face file page (`.../blob/main/model-Q4_K_M.gguf`) | Downloads that file. |
| A direct link to a `.gguf` file on any site | Downloads that file. |

Downloads go into `models_dir` and:

- **resume** if interrupted (the partial file is kept as `<name>.part`; cancelling also keeps it),
- are verified against Hugging Face's SHA-256 and expected size, and must start with the `GGUF` magic number (so a
  login page saved by mistake is rejected rather than kept),
- check that the drive has enough free space (plus a 1 GB margin) before starting,
- only ever write a plain file name inside `models_dir`; a name like `..\..\x.gguf` is refused.

Gated or private Hugging Face models need a token: set the `HF_TOKEN` environment variable (or
`HUGGING_FACE_HUB_TOKEN`) and restart the app. It is read from the environment only, never written to disk, and
is sent to `huggingface.co` hosts only, not to the download servers a request is redirected to.

**Starting settings.** When a download finishes, the app adds a `[[model]]` block with estimated settings, unless
you already configured that file. The estimate uses the model's size and layer count and your GPU's memory:
context starts at 32K (or the model's own limit if smaller); a model that fits in GPU memory runs fully on the GPU;
a Mixture-of-Experts model keeps just enough expert layers in RAM; a dense model that is too big offloads as many
layers as fit. These are **estimates, deliberately a little cautious**, calibrated against a 12 GB RTX 3060 running a
19.7 GB, 40-layer MoE model. Run **Auto-tune** to replace them with measured values for this machine.

### Default paths written on first run

| Setting | Windows | macOS | Linux |
|---|---|---|---|
| `llama_server` | a `llama-server` found on PATH or in a standard place, otherwise the Setup install folder: `%LOCALAPPDATA%\llm-switch\llama.cpp\llama-server.exe` | PATH, `/opt/homebrew/bin`, `/usr/local/bin`, otherwise `~/Library/Application Support/llm-switch/llama.cpp/llama-server` | PATH, `/usr/local/bin`, `~/llama.cpp/build/bin`, otherwise `~/.local/share/llm-switch/llama.cpp/llama-server` |
| `models_dir` | `C:\Users\<you>\models` | `~/models` | `~/models` |
| `log_file` | `%LOCALAPPDATA%\llm-switch\server.log` | `~/Library/Application Support/llm-switch/server.log` | `~/.local/share/llm-switch/server.log` |
| OpenCode config | `%USERPROFILE%\.config\opencode\opencode.json` | `~/.config/opencode/opencode.json` | same |

All of these can be changed in the config. If OpenCode is not installed (no `opencode.json`), the sync is simply
skipped.

### Config reference

```toml
llama_server = 'C:\Users\you\AppData\Local\llm-switch\llama.cpp\llama-server.exe'   # path to llama-server
models_dir   = 'C:\Users\you\models'              # every .gguf in here is listed (not recursive)
host         = "127.0.0.1"
port         = 8080
log_file     = 'C:\Users\you\AppData\Local\llm-switch\server.log'   # server output goes here; the Log tab shows its tail

[opencode]
config_path       = 'C:\Users\you\.config\opencode\opencode.json'
provider_id       = "llamacpp"     # key under "provider" in opencode.json
auto_sync         = true           # write context limits to opencode.json on every start
output_limit      = 8192           # "limit.output" written for the model
compaction_buffer = 10000          # tokens kept free before OpenCode compacts; remove the line to leave it alone

[defaults]                         # used by every model unless it overrides them (these are the built-in values)
ctx           = 32768              # -c
ngl           = 99                 # -ngl (layers on the GPU)
# n_cpu_moe   = 28                 # --n-cpu-moe (MoE models: experts of the first N layers stay in RAM); leave out for dense models
flash_attn    = true               # -fa on
cache_type_k  = "q8_0"             # -ctk
cache_type_v  = "q8_0"             # -ctv
jinja         = true               # --jinja (needed for tool calling)
extra_args    = []                 # anything else to pass to llama-server

[[model]]                          # one block per model you want to name or tune; all fields but `file` are optional
file        = "Qwen3.5-35B-A3B-heretic-Q4_K_M.gguf"
name        = "Qwen3.5 35B-A3B heretic"      # shown in the window and written to OpenCode
opencode_id = "qwen35-heretic"               # key under provider.<id>.models in opencode.json
ctx         = 65536                          # any [defaults] field can be overridden here
n_cpu_moe   = 25
```

A model with no `[[model]]` block still works: it is listed by its file name and runs on `[defaults]`.
Multimodal projector files (`mmproj*`), `mtp-*` helper files and split shards other than the first are hidden.

The flags the app passes to `llama-server` are built as:

```
-m <model> -c <ctx> -ngl <ngl> [--n-cpu-moe N] [-fa on] [-ctk K] [-ctv V] [--jinja] --host H --port P <extra_args>
```

`llm-switch-cli args` prints the exact command line for a model.

---

## 6. Using the app

- **Status** at the top: Stopped, Loading, or Ready (with the process ID and the address being served).
- **Model list**: click a model to select it. "running" marks the one being served.
- **Start / Stop / Switch to this model**: the button does exactly what its label says. It shows
  *Starting...* or *Stopping...* while it works. Stopping a large model can take several seconds.
- **Restart**: stop and start the selected model again, for example after editing the config.
- **Sync OpenCode**: write the model's limits to `opencode.json` without starting anything.
- **Reload config** / **Open config**.
- **Launch command**: expand to see the exact command line.
- **Server log** tab: live tail of the server's output. **Auto-tune** tab: see [Auto-tune](#8-auto-tune).
  **Setup** tab: install llama.cpp and download models, see [First run: the Setup tab](#first-run-the-setup-tab).
  When something is missing, an amber notice at the top has an **Open Setup** button.

The server is started as a separate process, so it keeps running if you close the window. Use **Stop** (or
`llm-switch-cli stop`) to shut it down.

---

## 7. Command-line reference

```
llm-switch-cli list                  models found in the models folder, with their settings
llm-switch-cli status                stopped, or running with pid, model and health
llm-switch-cli start  [model]        start a model (partial name match; default: the first one)
llm-switch-cli stop                  stop the server
llm-switch-cli toggle [model]        stop if that model is running, otherwise start it
llm-switch-cli sync   [model]        write the model's limits into opencode.json
llm-switch-cli args   [model]        print the llama-server command line
llm-switch-cli config                print the config file path
llm-switch-cli tune   [model] [--ctx 32768,65536,131072] [--headroom 700] [--apply]
llm-switch-cli hardware              what this machine has (OS, CPU, GPU, RAM)
llm-switch-cli setup                 what is installed and what is still missing
llm-switch-cli install-llama [--dir DIR] [--dry-run]
llm-switch-cli download LINK [--pick N] [--dry-run]
```

`install-llama` and `download` do the same as the Setup tab, with progress on the terminal. `--dry-run` shows what
would be downloaded (build, sizes, free space) without downloading anything. `download` with a repository link that
has several files prints a numbered list and exits; run it again with `--pick N`.

`[model]` is any part of the file name or display name. On Windows, the release GUI exe has no console, which is why
the command-line tool is a separate program.

---

## 8. Auto-tune

Finds the best settings for **this** machine by measuring, not guessing.

```
llm-switch-cli tune --ctx 65536,131072 --headroom 700 --apply
```

or use the **Auto-tune** tab in the window (pick contexts, set headroom, **Run auto-tune**, then **Apply recommended**).

**What it does**

1. Reads the model's layer count, expert count and trained context from the GGUF header.
2. Stops the running server and records how much VRAM other programs are using.
3. For each context size, restarts the server with trial settings and measures VRAM after a warm-up prompt:
   - **MoE models**: searches `--n-cpu-moe` (binary search) for the **smallest** value that still fits.
   - **Dense models**: searches `-ngl` for the **largest** value that still fits.
4. At the winning setting it measures **decode speed** (tok/s) and **prefill speed** on a long prompt, and backs off
   one step at a time if the long prompt pushes VRAM over the limit.
5. Recommends the **largest context whose decode speed is within 75 % of the fastest** one measured.
6. Restores the server the way it was (running again with your current settings, or stopped).

"Fits" means VRAM stays under `total - headroom` (default headroom 700 MiB), so there is room for Windows or the
desktop. A setting that crashes the server (out of VRAM) counts as not fitting.

**Things to know**

- It takes the server offline for roughly 5 to 15 minutes. The first load of a large model can take a few minutes
  if the file is not cached in RAM.
- **Close or idle OpenCode first.** The tuner refuses to start if the server is mid-request, and warns if another
  program is connected: a request arriving during a trial skews the speed numbers and is lost when the server restarts.
- `--apply` / **Apply recommended** writes `ctx` and `n_cpu_moe` (or `ngl`) into that model's `[[model]]` block
  (creating it if needed). Press **Restart** afterwards to use them. Saving rewrites `config.toml` without comments.
- Requires `nvidia-smi` (NVIDIA GPU) on **Windows and Linux**. On **macOS it is not available yet**: Apple Silicon
  shares one memory pool between CPU and GPU, so the VRAM-based search does not apply. It returns a clear message.

---

## 9. OpenCode integration

On every start (when `auto_sync = true`) and on **Sync OpenCode**, the app edits `opencode.json`:

```jsonc
{
  "compaction": { "auto": true, "buffer": 10000 },
  "provider": {
    "llamacpp": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "llama.cpp",
      "options": { "baseURL": "http://127.0.0.1:8080/v1" },
      "models": {
        "qwen35-heretic": {
          "name": "Qwen3.5 35B-A3B heretic",
          "limit": { "context": 131072, "output": 8192 }
        }
      }
    }
  }
}
```

- Everything else in the file is left alone, and a one-time backup is saved as `opencode.json.llm-switch.bak`.
- The file must be **plain JSON** (no comments or trailing commas). Otherwise the app refuses to touch it.
- Restart OpenCode after a sync so it re-reads the limits. `limit.context` always matches the server's `-c`.
- `compaction_buffer` controls how early OpenCode summarizes the conversation: it compacts at about
  `context - buffer`. Remove the line from the config to leave your own compaction settings alone.

---

## 10. Project layout

```
Cargo.toml              one library crate and two binaries
build.rs                embeds the Windows icon (only when the target is Windows)
icon.rc                 resource script used by build.rs
assets/icon.ico         Windows icon (multi-size)
assets/icon.icns        macOS icon (11 PNG entries)
packaging/
  make-icns.ps1         regenerates assets/icon.icns (run on Windows)
  macos/bundle.sh       builds "LLM Switch.app" and a zip
  macos/Info.plist.in   bundle metadata template (@VERSION@ is filled in)
.github/workflows/release.yml CI: builds Windows and macOS, picks the version, publishes the releases
packaging/ci/next-version.sh  Conventional Commits -> next version (also runnable locally)
packaging/ci/set-version.sh   stamps the version into Cargo.toml for a CI build
src/
  main.rs               the GUI program (llm-switch)
  bin/llm-switch-cli.rs the command-line program
  lib.rs                module list
  config.rs             config file, per-model settings, launch arguments
  server.rs             start, stop, health, HTTP helpers, log tail
  platform.rs           every OS-specific piece, behind cfg (paths, spawning, dialogs, VRAM, connections, GPU lookup)
  opencode.rs           opencode.json sync
  actions.rs            start/stop/toggle, install llama.cpp, download a model, "tune then restore the server"
  tune.rs               the auto-tuner
  gguf.rs               minimal GGUF header reader
  hardware.rs           detects OS, CPU, GPUs (NVIDIA driver and compute capability) and RAM
  download.rs           resumable, checksummed HTTP downloads; free-space check
  llamacpp.rs           picks, downloads, unpacks and verifies the right llama.cpp build
  models.rs             Hugging Face / direct links -> .gguf files on disk
  suggest.rs            estimated starting settings for a newly downloaded model
  gui.rs                the window (egui / eframe), including the Setup tab
```

Design rule: **no OS-specific code (`cfg` gates or system calls) outside `platform.rs`.** Add new OS-specific
behaviour there. `llamacpp.rs` does contain the names of llama.cpp's release files (`win`, `macos`, `ubuntu`), but
that is data about someone else's naming, kept as pure functions so it can be tested on any machine.

---

## 11. Platform support

| | Windows | macOS | Linux |
|---|---|---|---|
| Builds | yes, tested | **yes, in CI** (universal `arm64` + `x86_64`) | type-checks only (`x86_64-unknown-linux-gnu`) |
| Window, start / stop / sync | **tested** | not run | not run |
| Auto-tune | **tested** (NVIDIA) | not available yet | untested (needs `nvidia-smi`) |
| Setup: install llama.cpp, download a model | **tested**: installed the CUDA build, started it, downloaded and resumed a model | build selection unit-tested; nothing downloaded or run | build selection unit-tested; nothing downloaded or run |
| `.app` bundle / icon | n/a | **built in CI**, ad-hoc signature verifies; **not launched** | n/a |

---

## 12. Troubleshooting

| Symptom | Cause and fix |
|---|---|
| "llama-server not found at ..." | `llama_server` in the config points to the wrong place. Run `llm-switch-cli config`, edit it. |
| "no .gguf models found" | `models_dir` is wrong, or the models are in a subfolder (the scan is not recursive). |
| The window shows *Loading...* for a long time | Big models take a while the first time (the file is read from disk). Check the **Server log** tab. |
| Server exits right away | Read the log. Usually out of VRAM or RAM. Lower `ctx`, raise `n_cpu_moe`, or use a smaller quant. |
| Port already in use | Another program holds the port. Change `port` in the config (and Restart). |
| "the server is busy answering a request" (tuner) | Something is generating. Wait for it to finish or close OpenCode, then retry. |
| "... is not plain JSON" (OpenCode sync) | `opencode.json` has comments or trailing commas. Remove them, or set `auto_sync = false`. |
| OpenCode shows the wrong context | Restart OpenCode after a sync, and make sure `-c` and `limit.context` match (they do when the app writes both). |
| Config parse error on start | A typo in `config.toml`. The message names the line. Delete the file to regenerate the defaults. |
| Setup: "no ready-made llama.cpp build was found" | Your OS/CPU has no prebuilt release. Build llama.cpp from source and set `llama_server` in the config. |
| Setup: "could not start because a required DLL is missing" (Windows) | Install the [Microsoft Visual C++ Redistributable](https://aka.ms/vs/17/release/vc_redist.x64.exe), then try again. |
| Setup: "NVIDIA driver is too old for the CUDA builds" | The Vulkan build was installed instead. Update the NVIDIA driver (527 or newer) and reinstall for the faster CUDA build. |
| Download: HTTP 401 or 403 | Check the repository name for typos (Hugging Face reports a missing repository as 401). For a gated model, accept its terms on the website and set `HF_TOKEN`. |
| Download: "not a GGUF file" | The link led to a web page, not the model file. Use the repository link or the file's "resolve" link. |
| Download: "not enough free space" | The drive holding `models_dir` is too full. Free space, or point `models_dir` at another drive. |
| Download stopped halfway | Run the same download again: it resumes from `<name>.part`. |
| Setup install: "checksum mismatch" | The file was corrupted in transit and has been deleted. Try again. |
| macOS: "app is damaged" or "unidentified developer" | Gatekeeper quarantine. See [Signing, Gatekeeper and notarization](#signing-gatekeeper-and-notarization). |

---

## 13. Development notes

```bash
cargo build --release            # optimized build
cargo run --bin llm-switch       # run the GUI from source (a console window stays open in debug builds)
cargo run --bin llm-switch-cli -- status
```

### Automated builds and releases

The workflow `.github/workflows/release.yml` runs on GitHub's servers:

| Trigger | What happens |
|---|---|
| **Push to `main`** (a direct push, or a pull request being merged) | Builds Windows x64 and macOS universal, then recreates the rolling **pre-release `latest`** with both zips. If the commits since the last release call for it, it also publishes a **numbered release** and creates its `v` tag (see below). Pushes that only touch `*.md`, `LICENSE` or the git dotfiles are skipped. |
| **Push a `v*` tag by hand** | Builds the same files and publishes that tag as a release, exactly as named. |
| **Run manually** (Actions tab, "Run workflow") | Builds only. The zips are kept as workflow artifacts for 90 days; nothing is released. |

#### How the version is chosen

Versions follow [Semantic Versioning](https://semver.org) and are decided from
[Conventional Commit](https://www.conventionalcommits.org) messages. The workflow reads every commit since the last
`v*` tag, and the **highest** rule that matches wins:

| Commit message | Bump | Example: `v1.4.7` becomes |
|---|---|---|
| `type!: ...` or a `BREAKING CHANGE:` line in the body | **major** | `v2.0.0` |
| `feat: ...` or `feat(scope): ...` | **minor** | `v1.5.0` |
| `fix: ...` or `perf: ...` | **patch** | `v1.4.8` |
| `docs`, `chore`, `ci`, `test`, `style`, `refactor`, `build`, anything else | none | no numbered release |

Details worth knowing:

- The type must start the subject line (`feat: add X`). A `feat:` later in the line or only in the body does not count;
  a `BREAKING CHANGE:` footer in the body does. Types are case-insensitive.
- Merge commits and unknown types (`Merge pull request ...`, `feature:`) are ignored. **With squash-merge, the pull
  request title becomes the commit message, so title the PR like a commit** (`fix: handle empty config`). With a
  merge commit or rebase, the individual commits inside the PR are what count.
- Pushes that produce no bump still refresh `latest`, built as `<last version>-dev.<commit>`.
- **The first release:** there is no tag to bump from, so the first `feat`/`fix`/`perf` commit releases the version
  in `Cargo.toml` as it is (`v0.1.0`). From then on, the latest tag is the source of truth.
- `Cargo.toml` is **not** edited or committed by CI. It is stamped with the computed version on the build machine
  only, so the files in a release carry the right version while your source keeps its old number. After a release,
  `Cargo.toml` in `main` is just a starting point for the next first-time build; the tag is what counts.
- To force a specific version, push the tag yourself: `git tag v2.0.0 && git push origin v2.0.0`.
- The rules live in `packaging/ci/next-version.sh`, which you can run locally from the repository root to preview
  the next version: `bash packaging/ci/next-version.sh`.

Release assets:

- `LLM-Switch-<version>-windows-x64.zip`: `llm-switch.exe`, `llm-switch-cli.exe`, `LICENSE`, `README.md`.
- `LLM-Switch-<version>-macos.zip`: the universal `LLM Switch.app`.

Publishing runs in a single job after **both** builds succeed, so a failed Windows or macOS build never produces a
half-empty release. A newer push to the same branch cancels a build that is still running; nothing is lost, because
the next run reads every commit since the last release.

### Tests

```bash
cargo test --release
```

The suite has about 45 tests: the download engine against a local HTTP server (full download, resume, checksum
and size mismatch, cancel), llama.cpp build selection against real release file names, archive extraction
(including path-traversal entries), Hugging Face link parsing, hardware parsing, and the starting-settings estimate
checked against measurements from a real GPU. CI runs it on Windows and on macOS before anything is built or
published, so a failing test stops a release.

What the tests do **not** cover: the window itself, and anything that needs a real GPU or a real llama-server.
After a change to those, check by hand: `llm-switch-cli list`, `start`, `status`, `toggle`, and open the window.

### Checking another platform without it

The HTTPS library (`rustls`, through `ring`) compiles a little C, so `cargo check --target <mac or linux>` from a
Windows PC fails unless you have a C cross-compiler. Two ways around that:

- Let CI do it: the macOS job builds the real app on a Mac runner on every push to `main`.
- Type-check just the OS-specific modules, which have no C dependencies, in a scratch crate that includes
  `src/platform.rs`, `config.rs`, `gguf.rs`, `hardware.rs` and `suggest.rs` with `#[path]`. That is how the Unix
  branches of `platform.rs` were checked for macOS and Linux from Windows.

---

## 14. License

LLM Switch is open source under the **[MIT License](LICENSE)**. You may use, copy, modify, merge, publish,
distribute, sublicense and sell it, as long as the copyright notice and the license text stay with it.
The software is provided "as is", without warranty of any kind.

**Third-party code.** The program is built from about 320 Rust crates (listed in `Cargo.lock`). Every one of them is
under a permissive license (MIT, Apache-2.0, BSD, Zlib, ISC, Unlicense, BSL-1.0 or similar; a few that offer GPL or
LGPL terms also offer MIT or Apache-2.0, which is what applies here). The `egui` default-fonts bundle that is
compiled into the window also carries the SIL Open Font License 1.1 and the Ubuntu Font Licence. If you
redistribute built binaries, you may want to ship those notices with them. To regenerate the full list:

```bash
cargo install cargo-license
cargo license
```

**Not included.** LLM Switch does not bundle llama.cpp (MIT) or any model. Models are covered by their own licenses;
check the terms of each `.gguf` you download.

Contributions are welcome. By submitting a pull request you agree that your contribution is licensed under the
same MIT License.
