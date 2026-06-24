// port-zero Go example — stdlib net/http (no external dependencies)
//
// Usage:
//
//	PORT_ZERO=myapp-mybranch.devenv.local go run ./examples/
//
// PORT_ZERO must be set BEFORE starting this process. Setting it at
// runtime (os.Setenv) does NOT work for daemon discovery — the daemon reads
// /proc/<pid>/environ which is frozen at execve() time.
//
// Recommended: use direnv (see sdks/direnv/README.md) or shell export.
//
// The daemon on the host resolves {branch}/{worktree} templates; this
// process just needs to bind port 0 and inherit PORT_ZERO.
package main

import (
	"fmt"
	"net/http"
	"os"

	portzero "github.com/port-zero/port-zero/sdks/go"
)

func main() {
	// PORT_ZERO must already be set in the environment before this process started.
	ln, port, err := portzero.FindFreeListener(portzero.Options{
		ServiceName: "example-http",
	})
	if err != nil {
		fmt.Fprintf(os.Stderr, "error: %v\n", err)
		os.Exit(1)
	}

	mux := http.NewServeMux()
	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		tunnel := os.Getenv("PORT_ZERO")
		if tunnel == "" {
			tunnel = "(not set)"
		}
		fmt.Fprintf(w, "port-zero Go example\nPORT_ZERO: %s\nRequest: %s %s\n",
			tunnel, r.Method, r.URL.Path)
	})

	fmt.Printf("[example] Listening on http://0.0.0.0:%d\n", port)
	fmt.Println("[example] The port-zero daemon routes PORT_ZERO -> this port.")

	if err := http.Serve(ln, mux); err != nil {
		fmt.Fprintf(os.Stderr, "server error: %v\n", err)
		os.Exit(1)
	}
}
