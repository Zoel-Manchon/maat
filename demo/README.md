# Demo scenario

The demo intentionally includes a **safe external-tamper simulation**.
`demo/attacker.sh` is not an exploit and does not access the network. It only
waits for a configured delay and appends one clearly marked line to
`demo/sample.txt` while Maat is open.

That controlled change demonstrates Maat's central security feature:

1. Maat anchors the file's SHA-256 when it opens it.
2. The simulated attacker changes the file on disk.
3. `:w` is blocked because memory and disk no longer correspond.
4. `:check` reports the conflict.
5. The operator may inspect the situation or deliberately use `:w!`.
6. The forced save emits a structured audit event when `MAAT_AUDIT_LOG` is set.

Record the real terminal session from Linux, macOS or WSL:

```bash
./scripts/record-demo.sh
```

The script restores `demo/sample.txt` after recording.
