# Durable local Build runner (P2c, protocol slice)

Standalone Rust runner and SSH client; no Awili UI or usage-ledger wiring. This candidate has only been exercised with fake model programs and temporary local bare Git repositories. It has not been deployed to Hosta or used to call a model.

`awili-build-runner --config /absolute/config.json submit|status|logs|cancel` reads one JSON object from stdin and emits one JSON result. Errors are stderr/nonzero. `submit` accepts `BuildRequest`; status/cancel accept `{"request_id":"canonical-uuid"}`; logs adds `cursor` and `max_bytes` (capped at 65,536). The internal `--root PATH worker UUID` command only accepts the registered live worker process identity; it is not a retry command.

Configuration is administrator-owned JSON:

```json
{
  "job_root": "/configured/fleet-storage/awili-build/jobs",
  "targets": {
    "qwen-hosta": {
      "repo_id": "lumi",
      "repo_url": "ssh://git@10.77.68.86:2222/owner/lumi.git",
      "model": "qwen38-27b-nvfp4",
      "command": {
        "program": "/home/example/.npm-global/bin/dsh",
        "args": ["--profile", "headless", "--patch", "/home/example/overnight/dsh-overnight.patch.yml"],
        "env": {"DSH_PERMISSION_MODE": "danger-full-access"}
      },
      "verify_profile": "dispatch-tests",
      "verify": {
        "program": "/home/example/.cargo/bin/cargo",
        "args": ["test", "--locked", "-p", "awili-dispatch"],
        "env": {}
      }
    }
  }
}
```

This is an example, not an installed configuration. Resolve the storage root, executable paths, patch path, complete required PATH/environment and credentials on the deployment host before use. Existing host recipes also provide SPARK_API_KEY=local, explicit PATH, and CARGO_NET_GIT_FETCH_WITH_CLI=true; this shortened sample is not deploy-ready. Credentials remain in administrator environment/config, never request text. The runner overrides DSH_MODEL with the frozen target model and disables DSH telemetry. The request selects an existing target/repo/profile; it cannot supply command arguments, environment, verification commands or repository URLs. The exact spec is one final model argv element, never shell code. The root is private mode 0700; the runner and local coding process operate under the same account, so this is ownership discipline, not an adversarial sandbox.

Submission reserves a canonical UUID and branch under an OS advisory lock, persists the frozen request/config and snapshot, then launches a detached worker. Replays with the same UUID/payload return the existing job; conflicts fail. An SSH client timeout or lost acknowledgment reads status for the SAME UUID, never creates a new request. The SSH transport has a caller-supplied positive deadline covering process exit AND nonblocking stdin/stdout I/O, a bounded response, fixed safe remote paths and JSON stdin. It creates no reader/writer threads to join. If the direct process exits while a descendant retains a pipe, the call still reaches its deadline and closes its local handles. Cleanup never signals a reaped leader's potentially reused process-group ID. Ending the local SSH transport does not stop the remote worker.

The worker creates a new clone under its job directory, checks out the exact 40-character baseline, verifies the task branch did not already exist remotely, runs the configured model, commits owned changes, independently verifies the commit and clean checkout, then pushes with an empty-ref force-with-lease. This last condition prevents a competing remote branch creation from being overwritten. AwaitingReview requires successful model/verification/push exits and an exact remote SHA match. It does not mean merged, reviewed or deployed. The `executor` field records the actual configured executable path; `requested_model` records configuration, not an attestation of the backend's served model. An exit-zero no-op is reported as a failed implementation with `no_op=true`.

Snapshot updates use fsync + rename; locks are released by the OS on process death. The state revision increases monotonically. On observation, an active record whose worker identity cannot be established becomes NeedsAttention, never automatic retry or success. A crash before the first complete record can leave a reserved branch/job directory; that returns an ownership/read error and requires operator inspection. Do not delete such reservations and blindly resubmit. Failed/no-op jobs retain their checkout/logs for review. Automatic recovery-branch publication of failed work is not implemented; deployment must include an owner who checkpoints/pushes meaningful failed output under the fleet completion policy. No failure fixture is represented as a successful or remotely backed-up result.

Cancellation verifies worker creation identity and the UUID in its command before signalling its owned process group. It returns CancelRequested until group exit is observed, or NeedsAttention when ownership/delivery cannot be proved. Process groups do not contain descendants that deliberately detach into another session; Hosta containment must be reviewed before promising cancellation of every tool subprocess. Linux uses boot ID plus /proc start ticks; macOS uses process creation time plus the command UUID check. State/log files and administrator configuration are trusted, and external same-account tampering is outside this contract. There is no automatic cleanup/retry, server daemon, review dispatcher, UI binding, or ledger migration in this slice.

Tests use temporary local bare repositories and harmless scripts to prove literal spec/model delivery, duplicate/conflicting requests, ownership, remote publication races, model/verifier failure, no-op, restart/lost-worker observation, cautious cancellation, bounded SSH transport and lost-ack reconciliation. No fleet model endpoint is contacted.

Review corrections add a shared 640 KiB JSON wire cap (including worst-case escaping of the accepted 96 KiB raw spec), response UUID checks for status/cancel, and a baseline-ancestor check before verification/publication. The requested model remains configuration rather than backend attestation.
