Awili for Linux — early test build
===================================

This is an early build of Awili, a desktop AI workspace. It runs on your
computer and talks only to the AI services you connect yourself.

What you need
-------------
- A 64-bit (x86_64) Linux desktop with a recent glibc (built on Fedora 44;
  other distributions are untested).
- A graphics stack with Vulkan: a normal GPU driver, or Mesa's software
  Vulkan ("mesa-vulkan-drivers", "lavapipe") if you have none. The window
  stays blank without it.
- libdbus-1 (almost every desktop already has it).
- A Secret Service keyring, such as GNOME Keyring or KWallet, if you want
  Awili to remember API keys. Without one, Awili says so and does not save
  keys; it never writes them to disk on its own.
- At least one of:
  - Claude Code (from Anthropic) installed and signed in, or
  - Codex (from OpenAI) installed and signed in.
  These run the "Work" side of Awili. Install them the usual way (npm or the
  vendor installer) and sign in once from a terminal first.
- Optional: an OpenAI or Anthropic API key, for Council.

Installing and starting
-----------------------
1. Unpack this archive anywhere: tar xzf Awili-*-linux-x86_64.tar.gz
2. Run ./install.sh. It copies Awili into ~/.local/bin and adds an app-menu
   entry and icons under ~/.local/share. It does not use sudo.
3. Start "Awili" from your app menu, or run ~/.local/bin/awili-app.
4. The welcome screen checks for Claude Code or Codex. If it says Work isn't
   set up, install and sign in to one of them, then click "Check again".

Windows and displays
--------------------
- Tested under X11 only (a plain X server with a window manager). Wayland
  desktops normally run it through XWayland; native Wayland is untested.
- Without a window manager the first frame can stay black until the window
  is resized. Any normal desktop has one.
- Untested: high-DPI scaling, multiple monitors, input methods (IME).

Where it keeps things
---------------------
- Conversations and data: ~/.local/share/awili (or $XDG_DATA_HOME/awili)
- Settings: ~/.config/awili/config.toml (or $XDG_CONFIG_HOME/awili)
- Awili's own folders are readable only by you.
- API keys you type into Settings go to your keyring, not a file.
- Nothing is sent anywhere except to the AI services you set up. There is no
  telemetry and no automatic update check.

Known limits in this build
--------------------------
- There is no agent sandbox on Linux yet. Turning the sandbox on makes Awili
  refuse to start autonomous runs rather than run them uncontained. Keep Work
  in "Plan first" mode (top right) so you see what it intends before it acts,
  and read Claude Code's or Codex's own approval prompts.
- Shortcuts use Ctrl (Ctrl+K opens the palette). A few hints may still show
  Mac symbols.
- Dictation (speech to text) downloads a model the first time you use it.
- Not code-signed, and there is no package-manager build yet.

Removing it
-----------
Run ./uninstall.sh from this folder. It removes the app, the menu entry and
the icons, and leaves your data. Delete ~/.local/share/awili and
~/.config/awili to remove that too. Keys saved to your keyring are filed under
"dev.awili.Awili".

Feedback
--------
Send notes and screenshots to whoever gave you this build.
