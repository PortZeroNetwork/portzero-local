# PortZero Examples

These examples live in the separate `portzero-examples` repository. Clone it next to this repository or anywhere convenient:

```sh
git clone https://github.com/PortZeroNetwork/portzero-examples.git
cd portzero-examples
```

Each example sets `PZ_TUNNEL` so the local daemon can make the process or Docker container available at a `*.portzero.local` name.

## Python process

macOS or Linux:

```sh
cd python/process
PZ_TUNNEL="python-process.portzero.local:80" ./scripts/run.sh
```

Windows PowerShell:

```powershell
cd python\process
$env:PZ_TUNNEL = "python-process.portzero.local:80"
.\scripts\run.ps1
```

Open `http://python-process.portzero.local/` after the example starts.

## Python Docker container

macOS or Linux:

```sh
cd python/docker
PZ_TUNNEL="python-docker.portzero.local:80" ./scripts/run.sh
```

Windows PowerShell:

```powershell
cd python\docker
$env:PZ_TUNNEL = "python-docker.portzero.local:80"
.\scripts\run.ps1
```

This example requires Docker to be installed and running.

Open `http://python-docker.portzero.local/` after the example starts.

## Rust process

macOS or Linux:

```sh
cd rust/process
PZ_TUNNEL="rust-process.portzero.local:80" ./scripts/run.sh
```

Windows PowerShell:

```powershell
cd rust\process
$env:PZ_TUNNEL = "rust-process.portzero.local:80"
.\scripts\run.ps1
```

Open `http://rust-process.portzero.local/` after the example starts.

## Rust Docker container

macOS or Linux:

```sh
cd rust/docker
PZ_TUNNEL="rust-docker.portzero.local:80" ./scripts/run.sh
```

Windows PowerShell:

```powershell
cd rust\docker
$env:PZ_TUNNEL = "rust-docker.portzero.local:80"
.\scripts\run.ps1
```

This example requires Docker to be installed and running.

Open `http://rust-docker.portzero.local/` after the example starts.
