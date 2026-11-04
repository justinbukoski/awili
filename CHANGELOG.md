# Changes

## 0.3.0-rc.2 — 2026-11-04

- Codex autonomous app-server turns now use automatic approval review instead of rejecting every approval-required MCP call. Planning remains read-only. Thread and turn requests carry the current settings explicitly.
- Linux and Windows window Close now follows Quit, with confirmation while work is running. Previously, the platform Hide operation did nothing, leaving the window open. macOS keeps its existing background behavior.
- Native agent completion cleans up background processes that hold stderr open, bounds waiting for detached stderr holders, and prevents late cancellation from signaling a reaped leader.
- Native stderr is drained while retaining only the latest 64 KiB, preserving the latest error without unbounded buffering.
- Linux packages can explicitly include an optional integration helper; installation/removal manages it alongside the app. App-only distributions do not accidentally include adjacent private helpers.

Codex automatic-review settings were checked with Codex CLI 0.166.0. Compatibility with older CLIs is unverified. The legacy exec fallback does not offer the new app-server approval behavior. Automatic review and configured tool policies can still refuse a call; this change does not guarantee browser connection success or replace required user verification.
