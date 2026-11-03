Awili for Windows — early test build
=====================================

This is an early build of Awili, a desktop AI workspace. It runs entirely on
your PC and talks only to the AI services you connect yourself.

What you need
-------------
- Windows 10 or 11, 64-bit (x64), on an NTFS drive (the normal C: drive is fine).
  If your profile is on a FAT or exFAT drive Awili tells you so in a banner and
  will not save anything there.
- At least one of:
  - Claude Code (from Anthropic) installed and signed in, or
  - Codex (from OpenAI) installed and signed in.
  These run the "Work" side of Awili. Install them the usual way (npm or the
  vendor installer) and sign in once from a terminal before opening Awili.
  Awili finds them in %APPDATA%\npm, %LOCALAPPDATA%\Programs, your
  .local\bin folder and on PATH.
- Optional: an OpenAI or Anthropic API key, for Council (several models
  answering at once).

Starting it
-----------
1. Unzip this folder anywhere you like (for example Documents\Awili).
2. Double-click Awili.exe.
3. Windows may show "Windows protected your PC" (SmartScreen) because this test
   build is not code-signed. Click "More info", then "Run anyway".
4. The welcome screen checks for Claude Code or Codex. If it says Work isn't
   set up, install and sign in to one of them, then click "Check again".

Keys and menus
--------------
- Shortcuts use Ctrl: Ctrl+N new conversation, Ctrl+K command palette,
  Ctrl+, settings, Ctrl+F search, Ctrl+Z / Ctrl+Y undo and redo,
  Ctrl+C / Ctrl+V / Ctrl+A as usual, Ctrl+Q quit. Enter sends. Ctrl+? (or
  More > Keyboard shortcuts) shows the whole list.
- There is no menu bar. Click "More" at the bottom of the left rail: it holds
  Settings, Conversations, Keyboard shortcuts, New conversation, Save
  conversation as..., Find, Command palette, About Awili (with the version)
  and Quit. The More menu is not available during first-run setup; Ctrl+K and
  Ctrl+Q still work there.
- In a Code terminal, Ctrl keys go to the shell as usual (Ctrl+W, Ctrl+K,
  Ctrl+C...); Awili's own Ctrl shortcuts pause while a terminal has focus.
  Copy and paste in a terminal are Ctrl+Shift+C and Ctrl+Shift+V.

Where it keeps things
---------------------
- Settings and conversations: %APPDATA%\Awili
- Downloaded speech models and caches: %LOCALAPPDATA%\Awili
- API keys you type into Settings are stored in Windows Credential Manager,
  not in a file. Awili's own folders are locked so only your Windows account
  can read them. If a settings file cannot be locked that way, Awili shows a
  banner and ignores any keys written directly in it.
- Nothing is sent anywhere except to the AI services you set up. There is no
  telemetry and no automatic update check.

Known limits in this build
--------------------------
- The shell sandbox used on Mac is not available on Windows yet. Keep Work in
  "Plan first" mode (top right) so you see what it intends before it acts, and
  read Claude Code's or Codex's own approval prompts.
- The build is not code-signed, so SmartScreen warns the first time.
- Tested at 100% and 150% display scaling on one monitor. Multiple monitors
  and ARM PCs are untested.
- Dictation (speech to text) downloads a model the first time you use it; its
  hold-to-talk key setting still lists Mac key names.

Removing it
-----------
Delete the Awili folder, then %APPDATA%\Awili and %LOCALAPPDATA%\Awili. Keys
are listed in Credential Manager under "dev.awili.Awili".

Feedback
--------
Send notes and screenshots to Owner.
