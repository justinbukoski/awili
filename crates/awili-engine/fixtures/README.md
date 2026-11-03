# awili-engine wire fixtures (A2)

Raw captured streaming transcripts. Each scenario = `NN-name.request.json`
(exact request body) + `NN-name.response.sse` (raw bytes as received).
Fixtures are IMMUTABLE once a codec test depends on them; new captures get
new files. SYNTHETIC fixtures are labeled in the filename and carry a README.

Captured live 2026-08-06:
- `chat-completions/hostc-qwen3-8b/` — Ollama /v1, HOSTC (01 plain+usage,
  02 single tool, 03 parallel tools, 04 tool-result continuation,
  05 client cancellation/truncated stream, 06 SYNTHETIC malformed)
- `chat-completions/hostf-vllm-laguna/` — vLLM 0.25.1, Spark (01 plain
  w/ reasoning deltas + length stop, 02 fragmented tool call)
- `chat-completions/qwen3-coder-30b/` — 08 provider OOM error (real)

Gated/missing (see docs/engine-wire-contract.md §Gaps): OpenAI Responses,
Anthropic Messages (Owner-gated, paid), P7 context-limit live capture.

## Provenance machinery (A2 merge, arbitration 2026-08-06)

Set A's capture/metadata machinery adopted: the re-capture tool (since
2026-10-08 `awili-conformance --capture OUT_DIR [scenario ...]`, the Rust port of
`capture.py`; endpoint and model from `AWILI_A2_BASE` / `AWILI_A2_MODEL`; give it a
per-server directory, never `chat-completions/` itself, whose `manifest.json` is curated),
per-fixture `*.meta.json` (endpoint, model, captured_at, sse_event_count,
saw_done_sentinel, synthetic flag), and `chat-completions/manifest.json`.
Set B live payloads (Laguna-era fleet) are CANONICAL. Set A's 2026-07-26
deepseek-v4-flash captures are retained verbatim under
`chat-completions-legacy-deepseek-2026-07-26/` as named regression fixtures —
never the current fleet baseline (that endpoint no longer exists).

Captured live 2026-08-12 (Lightning/Switchyard deploy day +1):
- `chat-completions/hostf-vllm-lightning/` — vLLM, Spark :8001,
  nemotron-3.5-lightning NVFP4 (served ctx 131072). Emits `reasoning`
  deltas (NOT `reasoning_content`) — codec tolerance case.
  (plain-turn, single-tool-call, parallel-tool-calls,
  tool-result-continuation, error-bad-request)
- `chat-completions/mac-switchyard-agent/` — NeMo Switchyard v0.2 router,
  Mac :4000, fronting Lightning/Laguna. VERIFIED: every chunk carries the
  *served* model id (`nemotron-3.5-lightning` observed), so per-turn
  provenance survives the router. Same five scenarios.
