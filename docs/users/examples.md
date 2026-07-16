# PortZero Examples

For a zero-dependency first step, run `portzero demo`: it serves a built-in
page through a Local tunnel at `http://hello.portzero.local` with nothing to
clone or install. The examples below show the same `PZ_TUNNEL` mechanism on
real stacks.

These examples live in the separate `portzero-examples` repository. Clone it next to this repository or anywhere convenient:

```sh
git clone https://github.com/PortZeroNetwork/portzero-examples.git
cd portzero-examples
```

Each example sets `PZ_TUNNEL` so the local daemon can make the process or Docker Compose project available at a `*.portzero.local` name.

## TypeScript process

macOS or Linux:

```sh
cd nodejs-typescript/process
PZ_TUNNEL="nodejs-typescript-process.portzero.local:80" node app.ts
```

Windows PowerShell:

```powershell
cd nodejs-typescript\process
$env:PZ_TUNNEL = "nodejs-typescript-process.portzero.local:80"; node app.ts
```

Open `http://nodejs-typescript-process.portzero.local/` after the example starts.

## TypeScript Docker Compose

macOS or Linux:

```sh
cd nodejs-typescript/docker
PZ_TUNNEL="nodejs-typescript-docker.portzero.local:80" docker compose up --build
```

Windows PowerShell:

```powershell
cd nodejs-typescript\docker
$env:PZ_TUNNEL = "nodejs-typescript-docker.portzero.local:80"; docker compose up --build
```

This example requires Docker with Docker Compose to be installed and running.

Open `http://nodejs-typescript-docker.portzero.local/` after the example starts.

## Python process

macOS or Linux:

```sh
cd python/process
PZ_TUNNEL="python-process.portzero.local:80" uv run python app.py
```

Windows PowerShell:

```powershell
cd python\process
$env:PZ_TUNNEL = "python-process.portzero.local:80"; uv run python app.py
```

Open `http://python-process.portzero.local/` after the example starts.

## Python Docker Compose

macOS or Linux:

```sh
cd python/docker
PZ_TUNNEL="python-docker.portzero.local:80" docker compose up --build
```

Windows PowerShell:

```powershell
cd python\docker
$env:PZ_TUNNEL = "python-docker.portzero.local:80"; docker compose up --build
```

This example requires Docker with Docker Compose to be installed and running.

Open `http://python-docker.portzero.local/` after the example starts.

## Rust process

macOS or Linux:

```sh
cd rust/process
PZ_TUNNEL="rust-process.portzero.local:80" cargo run --quiet
```

Windows PowerShell:

```powershell
cd rust\process
$env:PZ_TUNNEL = "rust-process.portzero.local:80"; cargo run --quiet
```

Open `http://rust-process.portzero.local/` after the example starts.

## Rust Docker Compose

macOS or Linux:

```sh
cd rust/docker
PZ_TUNNEL="rust-docker.portzero.local:80" docker compose up --build
```

Windows PowerShell:

```powershell
cd rust\docker
$env:PZ_TUNNEL = "rust-docker.portzero.local:80"; docker compose up --build
```

This example requires Docker with Docker Compose to be installed and running.

Open `http://rust-docker.portzero.local/` after the example starts.

## C# process

macOS or Linux:

```sh
cd csharp/process
PZ_TUNNEL="csharp-process.portzero.local:80" dotnet run --no-launch-profile
```

Windows PowerShell:

```powershell
cd csharp\process
$env:PZ_TUNNEL = "csharp-process.portzero.local:80"; dotnet run --no-launch-profile
```

Open `http://csharp-process.portzero.local/` after the example starts.

## C# Docker Compose

macOS or Linux:

```sh
cd csharp/docker
PZ_TUNNEL="csharp-docker.portzero.local:80" docker compose up --build
```

Windows PowerShell:

```powershell
cd csharp\docker
$env:PZ_TUNNEL = "csharp-docker.portzero.local:80"; docker compose up --build
```

This example requires Docker with Docker Compose to be installed and running.

Open `http://csharp-docker.portzero.local/` after the example starts.
