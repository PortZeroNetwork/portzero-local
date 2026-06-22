// devenv-tunnel Go example — stdlib net/http (no external dependencies)
//
// Usage:
//
//	DEVENV_TUNNEL=myapp-mybranch.devenv.local go run ./examples/
//
// DEVENV_TUNNEL must be set BEFORE starting this process. Setting it at
// runtime (os.Setenv) does NOT work for daemon discovery — the daemon reads
// /proc/<pid>/environ which is frozen at execve() time.
//
// Recommended: use direnv (see sdks/direnv/README.md) or shell export.
//
// The daemon on the host resolves {branch}/{worktree} templates; this
// process just needs to bind port 0 and inherit DEVENV_TUNNEL.
package main

import (
	"fmt"
	"net/http"
	"os"

	devenvtunnel "github.com/devenv-tools/devenv-tunnel/sdks/go"
)

func main() {
	// DEVENV_TUNNEL must already be set in the environment before this process started.
	ln, port, err := devenvtunnel.FindFreeListener(devenvtunnel.Options{
		ServiceName: "example-http",
	})
	if err != nil {
		fmt.Fprintf(os.Stderr, "error: %v\n", err)
		os.Exit(1)
	}

	mux := http.NewServeMux()
	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		tunnel := os.Getenv("DEVENV_TUNNEL")
		if tunnel == "" {
			tunnel = "(not set)"
		}
		fmt.Fprintf(w, "devenv-tunnel Go example\nDEVENV_TUNNEL: %s\nRequest: %s %s\n",
			tunnel, r.Method, r.URL.Path)
	})

	fmt.Printf("[example] Listening on http://0.0.0.0:%d\n", port)
	fmt.Println("[example] The devenv-tunnel daemon routes DEVENV_TUNNEL -> this port.")

	if err := http.Serve(ln, mux); err != nil {
		fmt.Fprintf(os.Stderr, "server error: %v\n", err)
		os.Exit(1)
	}
}
