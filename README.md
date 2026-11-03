# Awili

Awili is a native desktop workspace for working with AI. You give it a folder
and a task, and it runs the work with the AI tools you already use, showing you
each step and asking before it does anything risky. It is written in Rust with
the GPUI toolkit, so it is a real native app (no browser or web view inside),
and it runs on macOS, Windows and Linux.

This is a release candidate (`0.3.0-rc.1`). It works day to day for its author,
but expect rough edges.

| Windows | Linux |
|---|---|
| ![Awili on Windows 11: the main window with the More menu open](docs/screenshots/windows-menu.png) | ![Awili on Linux: the first-run welcome screen](docs/screenshots/linux-welcome.png) |

## What it does

- **Workshop**: a conversation tied to a folder, in a Work mode (an agent acts on
  the folder) or a Code mode. You see the plan and the tool calls, and approve
  or refuse risky steps.
- **Council**: send one message to several models at once (for example Claude
  Code, Codex, or an in-app model) and read their answers side by side.
- **Backstage**: Skills, Automations, Sessions, Connections and Health pages.
- **Design** is a placeholder for a later release.
- **Bring your own AI**. Awili does not include a model and has no account
  system. In Settings you add a provider (OpenAI, Anthropic or an
  OpenAI-compatible server such as Ollama) with your API key; the key is stored
  in the macOS Keychain, Windows Credential Manager or Linux Secret Service,
  and these providers power Council and Chat. Work needs Claude Code, Codex or
  a model server you run, installed and signed in by you.
- **Opt-in write sandbox (macOS only)**: an off-by-default setting wraps agent
  commands in a macOS sandbox profile that limits where they can write. It
  guards against mistakes; it is not a security boundary against a hostile
  agent. Windows and Linux have no agent sandbox yet.

## Privacy

Awili has **no telemetry, analytics or crash reporting**, and it does not phone
home. It talks only to the model servers and tools you configure. Your
conversations and settings are stored on your own computer. API keys you type in Settings
are kept in your operating system secret store (or come from environment
variables or files you name); they are not sent anywhere except to the service
you set them up for. Optional integrations (an optional external memory service, Outline and Gitea connectors, a "needs you" feed) are off
until you configure them.

## Install

Download the file for your system from the Releases page.

| System | Status | File |
|---|---|---|
| macOS (universal) | Release candidate, notarized | `Awili-0.3.0-rc.1-macos-universal.dmg` |
| Linux (x86_64) | Release candidate | `Awili-0.3.0-rc.1-linux-x86_64.tar.gz` |
| Windows 10/11 (x64) | **Preview**, unsigned | `Awili-0.3.0-rc.1-windows-x64.zip` |

**macOS (Apple Silicon and Intel), release candidate.** Open `Awili-0.3.0-rc.1-macos-universal.dmg` and
drag Awili to Applications. The app is signed and notarized by Apple, so it
opens normally.

**Windows 10/11 (x64), Preview.** Unzip `Awili-0.3.0-rc.1-windows-x64.zip` anywhere and run
`Awili.exe`. The build is **not code-signed**, so Windows SmartScreen will
say "Windows protected your PC". Choose **More info**, then **Run anyway**. (If
you would rather not, build it from source; see below.)

**Linux (x86_64), release candidate.** Unpack `Awili-0.3.0-rc.1-linux-x86_64.tar.gz` and run
`./install.sh` (or run `./awili-app` directly). You need a Wayland or X11
desktop with Vulkan drivers installed (built on Fedora 44; other distributions are
untested). To have Awili remember API keys you also need a Secret Service
keyring such as GNOME Keyring or KWallet.

First launch shows a welcome screen. Pick how you want to connect (an installed
agent such as Claude Code or Codex, or a model server or API key) and follow
the prompts. A sample configuration with every option explained is in
[`config.example.toml`](config.example.toml); the settings screen edits the same file.

## Known limits

- Release candidate software; formats and settings can still change.
- The first-run welcome screen has no key form: "Set up connection" opens
  Settings, where you set up providers (with API keys), choose an installed
  agent such as Claude Code or Codex, or scan your network for local model
  servers. The in-app engine endpoint is set in the config file.
- **Windows is a Preview.** It builds natively and is smoke-tested (welcome
  screen, focus, Ctrl shortcuts, owner-only file permissions, keys in Credential
  Manager), but about 500 of the roughly 3,000 app tests depend on Unix-only
  test fixtures and do not run on Windows yet. It is unsigned (SmartScreen:
  "More info", then "Run anyway"), has no agent sandbox, and is x64 only.
- Linux has been tested in Fedora 44 containers under Xvfb, not on a real
  desktop; Wayland and real-GPU setups are untested. There is no agent sandbox
  on Linux either.
- Voice input uses a local Whisper model that downloads on first use.
- Some settings and integrations in the code were built for the author's home
  network setup. They are off by default and harmless to ignore.

## Verifying downloads

SHA-256 of the 0.3.0-rc.1 files:

```
e7dab1119998d9505ac4d77e33ff91eb24800e54eacee029ef800b6a8b1b2a5b  Awili-0.3.0-rc.1-macos-universal.dmg
479b5e2fa187b8c123337cd271367d2bcadae6120682331da52c55b9a0df8314  Awili-0.3.0-rc.1-windows-x64.zip
9597c7ae49d9f118cfe35c6d6ba8945900f3085f77b52675d3ed3e54ae0aa2a6  Awili-0.3.0-rc.1-linux-x86_64.tar.gz
```

Check with `shasum -a 256 <file>` (macOS, Linux) or `Get-FileHash <file>`
(Windows PowerShell). The macOS app is notarized by Apple under the Developer
ID "Justin Bukoski".

## Build from source

You need:

- Rust, installed through [rustup](https://rustup.rs). The pinned version in
  `rust-toolchain.toml` is fetched automatically.
- **LLVM/clang 20** (libclang), used by the bindgen steps. Set
  `LIBCLANG_PATH` to its `lib` directory if it is not found, for example
  `/usr/lib64/llvm20/lib64` on Fedora. clang 20 is what the project is tested with.
- CMake and a C/C++ compiler (the voice feature builds whisper.cpp).
- macOS: Xcode with the **Metal toolchain** installed (Xcode 26 ships it as a
  separate component: `xcodebuild -downloadComponent MetalToolchain`).
- Linux: development packages for ALSA, fontconfig, freetype, Wayland, X11/xcb,
  xkbcommon, Vulkan, OpenSSL, libsecret, dbus, PipeWire and libdrm (Fedora
  names: `alsa-lib-devel fontconfig-devel freetype-devel wayland-devel
  libxcb-devel libxkbcommon-x11-devel vulkan-loader-devel openssl-devel
  libsecret-devel dbus-devel pipewire-devel libdrm-devel`).

```
git clone https://github.com/justinbukoski/awili.git
cd awili
cargo build --release -p awili-app
cargo test --workspace
```

The first build downloads a patched copy of GPUI and a few of its dependencies
from `github.com/justinbukoski` (see `Cargo.toml`, `[patch]`). On macOS,
`scripts/bundle-mac.sh` wraps the binary into `Awili.app`.

## Contributing and security

See [CONTRIBUTING.md](CONTRIBUTING.md) and [SECURITY.md](SECURITY.md).

## License

Dual licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option. Copyright 2026 Justin Bukoski.
