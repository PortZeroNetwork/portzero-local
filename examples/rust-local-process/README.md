# Rust Local Process Example

This example runs a native Windows-friendly Rust process that opts into Port
Zero with `PZ_TUNNEL`, binds to port `0`, and serves plain HTTP on the random
port assigned by the OS.

Run it from PowerShell:

```powershell
cd examples\rust-local-process
$env:PZ_TUNNEL = "rust-demo.portzero.local:8080"
cargo run
```

In another terminal, start or inspect the daemon:

```powershell
cargo run --manifest-path ..\..\client\crates\cli\Cargo.toml -- start
cargo run --manifest-path ..\..\client\crates\cli\Cargo.toml -- status
```

Direct traffic should always work with the real port printed by the server:

```powershell
curl.exe http://127.0.0.1:<printed-port>/
```

The full `.portzero.local` overlay path on Windows requires Administrator
privileges and `wintun.dll` next to the binary or in `PATH`.
